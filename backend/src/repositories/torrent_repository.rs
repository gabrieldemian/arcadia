use crate::{
    Error, Result,
    models::{
        torrent::{Features, Torrent, TorrentSearch, TorrentToDelete, UploadedTorrent},
        user::User,
    },
};

use bip_metainfo::{Info, InfoBuilder, InfoHash, Metainfo, MetainfoBuilder, PieceLength};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::str::FromStr;

use super::notification_repository::notify_users;

#[derive(sqlx::FromRow)]
struct TitleGroupInfoLite {
    id: i64,
    name: String,
}

pub async fn create_torrent(
    pool: &PgPool,
    torrent_form: &UploadedTorrent,
    current_user: &User,
) -> Result<Torrent> {
    let mut tx = pool.begin().await?;

    let create_torrent_query = r#"
        INSERT INTO torrents (
            edition_group_id, created_by_id, release_name, release_group, description,
            file_amount_per_type, uploaded_as_anonymous, file_list, mediainfo, trumpable,
            staff_checked, size, duration, audio_codec, audio_bitrate, audio_bitrate_sampling,
            audio_channels, video_codec, features, subtitle_languages, video_resolution, container,
            languages, info_hash, info_dict
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12, $13,
            $14::audio_codec_enum, $15, $16::audio_bitrate_sampling_enum,
            $17::audio_channels_enum, $18::video_codec_enum, $19::features_enum[], $20::language_enum[], $21, $22, $23::language_enum[], $24::bytea, $25::bytea
        )
        RETURNING *
    "#;

    let metainfo = Metainfo::from_bytes(&torrent_form.torrent_file.data)
        .map_err(|_| Error::TorrentFileInvalid)?;

    let info = metainfo.info();

    // We cannot trust that the uploader has set the private field properly,
    // so we need to recreate the info db with it forced, which requires a
    // recomputation of info hash
    let info_normalized = InfoBuilder::new()
        .set_private_flag(Some(true))
        .set_piece_length(PieceLength::Custom(info.piece_length() as usize))
        .build(1, info, |_| {})
        .map_err(|_| Error::TorrentFileInvalid)?;

    let info_hash = InfoHash::from_bytes(&info_normalized);

    // TODO: torrent metadata extraction should be done on the client side
    let parent_folder = info.directory().map(|d| d.to_str().unwrap()).unwrap_or("");
    let files = info
        .files()
        .map(|f| json!({"name": f.path().to_str().unwrap(), "size": f.length()}))
        .collect::<Vec<_>>();

    let file_list = json!({"parent_folder": parent_folder, "files": files});

    let file_amount_per_type = json!(
        info.files()
            .flat_map(|file| file.path().to_str().unwrap().split('.').next_back())
            .fold(std::collections::HashMap::new(), |mut acc, ext| {
                *acc.entry(ext.to_string()).or_insert(0) += 1;
                acc
            })
    );

    // TODO: check if the torrent is trumpable: via a service ?
    let trumpable = String::from("");
    let size = metainfo
        .info()
        .files()
        .map(|file| file.length())
        .sum::<u64>() as i64;

    let uploaded_torrent = sqlx::query_as::<_, Torrent>(create_torrent_query)
        .bind(torrent_form.edition_group_id.0)
        .bind(current_user.id)
        .bind(&*torrent_form.release_name.0)
        .bind(torrent_form.release_group.as_deref())
        .bind(torrent_form.description.as_deref())
        .bind(&file_amount_per_type)
        .bind(torrent_form.uploaded_as_anonymous.0)
        .bind(&file_list)
        .bind(&*torrent_form.mediainfo.0)
        .bind(&trumpable)
        .bind(false)
        .bind(size)
        .bind(torrent_form.duration.as_deref())
        .bind(torrent_form.audio_codec.as_deref())
        .bind(torrent_form.audio_bitrate.as_deref())
        .bind(torrent_form.audio_bitrate_sampling.as_deref())
        .bind(torrent_form.audio_channels.as_deref())
        .bind(torrent_form.video_codec.as_deref())
        .bind(
            torrent_form
                .features
                .split(',')
                .filter(|f| !f.is_empty())
                .map(|f| Features::from_str(f).ok().unwrap())
                .collect::<Vec<Features>>(),
        )
        .bind(
            torrent_form
                .subtitle_languages
                .0
                .split(',')
                .filter(|f| !f.is_empty())
                .map(|f| f.trim())
                .collect::<Vec<&str>>(),
        )
        .bind(torrent_form.video_resolution.as_deref())
        .bind(&*torrent_form.container)
        .bind(
            torrent_form
                .languages
                .0
                .split(',')
                .filter(|f| !f.is_empty())
                .map(|f| f.trim())
                .collect::<Vec<&str>>(),
        )
        .bind(info_hash.as_ref())
        .bind(info.to_bytes())
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::CouldNotCreateTorrent)?;

    let title_group_info = sqlx::query_as!(
        TitleGroupInfoLite,
        r#"
            SELECT title_groups.id, title_groups.name
            FROM edition_groups
            JOIN title_groups ON edition_groups.title_group_id = title_groups.id
            WHERE edition_groups.id = $1
        "#,
        torrent_form.edition_group_id.0
    )
    .fetch_one(&mut *tx)
    .await?;

    let _ = notify_users(
        &mut tx,
        "torrent_uploaded",
        &title_group_info.id,
        "New torrent uploaded subscribed title group",
        &format!(
            "New torrent uploaded in title group \"{}\"",
            title_group_info.name
        ),
    )
    .await;

    tx.commit().await?;

    Ok(uploaded_torrent)
}

pub struct GetTorrentResult {
    pub title: String,
    pub file_contents: Vec<u8>,
}

pub async fn get_torrent(
    pool: &PgPool,
    user: &User,
    torrent_id: i64,
    tracker_name: &str,
    frontend_url: &str,
    tracker_url: &str,
) -> Result<GetTorrentResult> {
    let mut tx = pool.begin().await?;

    let torrent = sqlx::query!(
        r#"
        UPDATE torrents
        SET snatched = snatched + 1
        WHERE id = $1
        RETURNING
            info_dict,
            EXTRACT(EPOCH FROM created_at)::BIGINT AS "created_at_secs!",
            release_name;
        "#,
        torrent_id
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| Error::TorrentFileInvalid)?;

    let info = Info::from_bytes(torrent.info_dict).map_err(|_| Error::TorrentFileInvalid)?;

    let tracker_url = {
        let passkey =
            ((user.passkey_upper as u64 as u128) << 64) | (user.passkey_lower as u64 as u128);

        format!("{}announce/{:x}", tracker_url, passkey)
    };

    let frontend_url = format!("{}torrent/{}", frontend_url, torrent_id);

    let metainfo = MetainfoBuilder::new()
        .set_main_tracker(Some(&tracker_url))
        .set_creation_date(Some(torrent.created_at_secs))
        .set_comment(Some(&frontend_url))
        .set_created_by(Some(tracker_name))
        .set_piece_length(PieceLength::Custom(info.piece_length() as usize))
        .set_private_flag(Some(true))
        .build(1, &info, |_| {})
        .map_err(|_| Error::TorrentFileInvalid)?;

    let _ = sqlx::query!(
        r#"
            INSERT INTO torrent_activities(torrent_id, user_id, snatched_at)
            VALUES ($1, $2, NOW())
            ON CONFLICT (torrent_id, user_id) DO NOTHING;
            "#,
        torrent_id,
        user.id,
    )
    .execute(&mut *tx)
    .await
    .map_err(|_| Error::InvalidUserIdOrTorrentId);

    tx.commit().await?;

    Ok(GetTorrentResult {
        title: torrent.release_name,
        file_contents: metainfo,
    })
}

pub async fn search_torrents(pool: &PgPool, torrent_search: &TorrentSearch) -> Result<Value> {
    let search_results = sqlx::query!(
        r#"
        WITH title_group_data AS (
            SELECT
                tgl.title_group_data AS lite_title_group
            FROM get_title_groups_and_edition_group_and_torrents_lite($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) tgl
        )
        SELECT jsonb_agg(lite_title_group) AS title_groups
        FROM title_group_data;
        "#,
        torrent_search.title_group.name,
        torrent_search.torrent.staff_checked,
        torrent_search.torrent.reported,
        torrent_search.title_group.include_empty_groups,
        torrent_search.sort_by.to_string(),
        torrent_search.order.to_string(),
        torrent_search.page_size,
        (torrent_search.page - 1) * torrent_search.page_size,
        torrent_search.torrent.created_by_id,
        torrent_search.torrent.snatched_by_id,
    )
    .fetch_one(pool)
    .await
    .map_err(|error| Error::ErrorSearchingForTorrents(error.to_string()))?;

    Ok(serde_json::json!({"title_groups": search_results.title_groups}))
}

pub async fn find_top_torrents(pool: &PgPool, period: &str, amount: i64) -> Result<Value> {
    let search_results = sqlx::query!(
        r#"
        WITH title_group_search AS (
            ---------- This is the part that selects the top torrents
            SELECT DISTINCT ON (tg.id) tg.id AS title_group_id
            FROM torrents t
            JOIN torrent_activities st ON t.id = st.torrent_id
            JOIN edition_groups eg ON t.edition_group_id = eg.id
            JOIN title_groups tg ON eg.title_group_id = tg.id
            WHERE CASE
                WHEN $1 = 'all time' THEN TRUE
                ELSE t.created_at >= NOW() - CAST($1 AS INTERVAL)
            END
            GROUP BY tg.id, tg.name
            ORDER BY tg.id, COUNT(st.torrent_id) DESC
            LIMIT $2
            ----------
        ),
        title_group_data AS (
            SELECT
                tgl.title_group_data AS lite_title_group -- 'affiliated_artists' is already inside tgl.title_group_data
            FROM get_title_groups_and_edition_group_and_torrents_lite() tgl
            JOIN title_groups tg ON tgl.title_group_id = tg.id
            JOIN title_group_search tgs ON tg.id = tgs.title_group_id
        )
        SELECT jsonb_agg(lite_title_group) AS title_groups
        FROM title_group_data;
        "#,
        period,
        amount
    )
    .fetch_one(pool)
    .await
    .map_err(|error| Error::ErrorSearchingForTorrents(error.to_string()))?;

    Ok(serde_json::json!({"title_groups": search_results.title_groups}))
}

pub async fn remove_torrent(
    pool: &PgPool,
    torrent_to_delete: &TorrentToDelete,
    current_user_id: i64,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    notify_users(
        &mut tx,
        "torrent_deleted",
        &0,
        "Torrent deleted",
        torrent_to_delete.displayed_reason.as_ref().unwrap(),
    )
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO deleted_torrents (SELECT *, NOW() AS deleted_at, $1 AS deleted_by_id, $2 AS reason FROM torrents WHERE id = $3);
        "#,
        current_user_id,
        torrent_to_delete.reason,
        torrent_to_delete.id
    )
    .execute(&mut *tx)
    .await
    .map_err(|error| Error::ErrorDeletingTorrent(error.to_string()))?;

    sqlx::query!(
        r#"
        DELETE FROM torrents WHERE id = $1;
        "#,
        torrent_to_delete.id
    )
    .execute(&mut *tx)
    .await
    .map_err(|error| Error::ErrorDeletingTorrent(error.to_string()))?;

    tx.commit().await?;

    Ok(())
}

pub async fn update_torrent_seeders_leechers(pool: &PgPool) -> Result<()> {
    let _ = sqlx::query!(
        r#"
        WITH peer_counts AS (
            SELECT
                torrent_id,
                COUNT(CASE WHEN status = 'seeding' THEN 1 END) AS current_seeders,
                COUNT(CASE WHEN status = 'leeching' THEN 1 END) AS current_leechers
            FROM
                peers
            GROUP BY
                torrent_id
        )
        UPDATE torrents AS t
        SET
            seeders = COALESCE(pc.current_seeders, 0),
            leechers = COALESCE(pc.current_leechers, 0)
        FROM
            torrents AS t_alias -- Use an alias for the table in the FROM clause to avoid ambiguity
        LEFT JOIN
            peer_counts AS pc ON t_alias.id = pc.torrent_id
        WHERE
            t.id = t_alias.id;
        "#
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn increment_torrent_completed(pool: &PgPool, torrent_id: i64) -> Result<()> {
    let _ = sqlx::query!(
        r#"
        UPDATE torrents
        SET
            completed = completed + 1
        WHERE
            id = $1
        "#,
        torrent_id
    )
    .execute(pool)
    .await?;

    Ok(())
}
