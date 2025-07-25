<template>
  <div v-if="conversation">
    <div class="title">{{ conversation.subject }}</div>
    <ConversationMessages :messages="conversation.messages" />
    <Form @submit="sendMessage">
      <BBCodeEditor
        :label="t('conversation.message')"
        :emptyInput
        @valueChange="(val: string) => (newMessage.content = val)"
        @inputEmptied="emptyInput = false"
      >
        <template #buttons>
          <Button type="submit" :label="t('general.send')" icon="pi pi-send" :loading="sendingMessage" />
        </template>
      </BBCodeEditor>
    </Form>
  </div>
</template>

<script lang="ts" setup>
import { getConversation, postConversationMessage, type ConversationHierarchy, type UserCreatedConversationMessage } from '@/services/api/conversationService'
import { Form } from '@primevue/forms'
import { onMounted } from 'vue'
import { ref } from 'vue'
import { useRoute } from 'vue-router'
import ConversationMessages from '@/components/conversation/conversationMessages.vue'
import BBCodeEditor from '@/components/community/BBCodeEditor.vue'
import { Button } from 'primevue'
import { useI18n } from 'vue-i18n'
import { useUserStore } from '@/stores/user'

const route = useRoute()
const { t } = useI18n()
const userStore = useUserStore()

const conversation = ref<ConversationHierarchy>()
const sendingMessage = ref(false)
const newMessage = ref<UserCreatedConversationMessage>({
  content: '',
  conversation_id: 0,
})
const emptyInput = ref(false)

const fetchConversation = async (conversationId: number) => {
  getConversation(conversationId).then((c) => {
    conversation.value = c
  })
}

const sendMessage = async () => {
  sendingMessage.value = true
  newMessage.value.conversation_id = parseInt(route.params.id as string)
  postConversationMessage(newMessage.value)
    .then((message) => {
      conversation.value?.messages.push({ ...message, created_by: userStore })
      emptyInput.value = true
    })
    .finally(() => {
      sendingMessage.value = false
    })
}

onMounted(() => {
  fetchConversation(parseInt(route.params.id as string))
})
</script>
<style scoped>
.messages {
  margin-bottom: 15px;
}
</style>
