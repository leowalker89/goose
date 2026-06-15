import type { GooseSessionNotification_unstable } from '@aaif/goose-sdk';
import type {
  ContentBlock as AcpContentBlock,
  RequestPermissionRequest,
  SessionNotification,
  ToolCall,
  ToolCallUpdate,
} from '@agentclientprotocol/sdk';
import type {
  CallToolResponse,
  ContentBlock as ApiContentBlock,
  Message,
  MessageContent,
  TokenState,
} from '../api';

export type AcpChatUpdate =
  | { type: 'messages'; messages: Message[] }
  | { type: 'tokenState'; tokenState: Partial<TokenState> }
  | { type: 'sessionInfo'; name?: string };

interface AdapterState {
  messages: Message[];
}

interface GooseMessageMeta {
  messageId?: string;
  created?: number;
}

const DEFAULT_VISIBLE_MESSAGE_METADATA: Message['metadata'] = {
  userVisible: true,
  agentVisible: true,
};

export interface AcpSessionNotificationAdapter {
  apply(notification: SessionNotification): AcpChatUpdate[];
  applyGoose(notification: GooseSessionNotification_unstable): AcpChatUpdate[];
  applyPermissionRequest(request: RequestPermissionRequest): AcpChatUpdate[];
  getMessages(): Message[];
}

export function createAcpSessionNotificationAdapter(
  initialMessages: Message[] = []
): AcpSessionNotificationAdapter {
  const state: AdapterState = {
    messages: initialMessages.map(cloneMessage),
  };

  return {
    apply(notification) {
      return applyAcpSessionNotification(state, notification);
    },
    applyGoose(notification) {
      return applyGooseSessionNotification(state, notification);
    },
    applyPermissionRequest(request) {
      return applyPermissionRequest(state, request);
    },
    getMessages() {
      return state.messages.map(cloneMessage);
    },
  };
}

function applyAcpSessionNotification(
  state: AdapterState,
  notification: SessionNotification
): AcpChatUpdate[] {
  const update = notification.update;

  switch (update.sessionUpdate) {
    case 'user_message_chunk':
      return applyContentChunk(state, 'user', update);
    case 'agent_message_chunk':
      return applyContentChunk(state, 'assistant', update);
    case 'agent_thought_chunk':
      return applyThoughtChunk(state, update);
    case 'tool_call':
      return applyToolCall(state, update);
    case 'tool_call_update':
      return applyToolCallUpdate(state, update);
    case 'session_info_update':
      return [
        {
          type: 'sessionInfo',
          ...(update.title ? { name: update.title } : {}),
        },
      ];
    case 'usage_update':
      return [];
    default:
      return [];
  }
}

function applyGooseSessionNotification(
  state: AdapterState,
  notification: GooseSessionNotification_unstable
): AcpChatUpdate[] {
  const update = notification.update;

  switch (update.sessionUpdate) {
    case 'usage_update':
      return [
        {
          type: 'tokenState',
          tokenState: {
            totalTokens: update.used,
            accumulatedInputTokens: update.accumulatedInputTokens,
            accumulatedOutputTokens: update.accumulatedOutputTokens,
            accumulatedTotalTokens: update.accumulatedInputTokens + update.accumulatedOutputTokens,
            ...(update.accumulatedCost !== undefined
              ? { accumulatedCost: update.accumulatedCost }
              : {}),
          },
        },
      ];
    case 'status_message':
      return applyStatusMessage(state, notification.sessionId, update);
    default:
      return [];
  }
}

function applyStatusMessage(
  state: AdapterState,
  sessionId: string,
  update: Extract<GooseSessionNotification_unstable['update'], { sessionUpdate: 'status_message' }>
): AcpChatUpdate[] {
  const notificationType = update.status.type === 'notice' ? 'inlineMessage' : 'thinkingMessage';

  state.messages.push({
    id: `acp_status_${sessionId}_${Date.now()}_${Math.random().toString(36).slice(2, 10)}`,
    role: 'assistant',
    created: Math.floor(Date.now() / 1000),
    content: [
      {
        type: 'systemNotification',
        notificationType,
        msg: update.status.message,
      },
    ],
    metadata: {
      userVisible: true,
      agentVisible: false,
    },
  });

  return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
}

function applyToolCall(state: AdapterState, update: ToolCall): AcpChatUpdate[] {
  const gooseMeta = getGooseMessageMeta(update);
  const message = getOrCreateAssistantMessageForUpdate(state, gooseMeta);

  const existing = message.content.find(
    (content) => content.type === 'toolRequest' && content.id === update.toolCallId
  );
  if (existing) {
    return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
  }

  const identity = toolIdentity(update);
  const metadata = toolRequestMetadata(update, identity);

  message.content.push({
    type: 'toolRequest',
    id: update.toolCallId,
    toolCall: {
      status: 'success',
      value: {
        name: identity.toolName ?? update.title,
        arguments: rawInputToArguments(update.rawInput),
      },
    },
    ...(metadata ? { metadata } : {}),
    ...(update._meta ? { _meta: update._meta } : {}),
  });

  return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
}

function applyToolCallUpdate(state: AdapterState, update: ToolCallUpdate): AcpChatUpdate[] {
  if (update.status !== 'completed' && update.status !== 'failed') {
    return [];
  }

  if (messageWithToolResponse(state, update.toolCallId)) {
    return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
  }

  const gooseMeta = getGooseMessageMeta(update);
  const message = getOrCreateToolResponseMessageForUpdate(state, gooseMeta);
  const identity = toolIdentity(update);
  const metadata = toolResponseMetadata(update, identity);

  message.content.push({
    type: 'toolResponse',
    id: update.toolCallId,
    toolResult:
      update.status === 'failed'
        ? { status: 'error', error: toolError(update) }
        : { status: 'success', value: toolResultValue(update, mcpAppMetadata(update)) },
    ...(metadata ? { metadata } : {}),
  });

  return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
}

function applyPermissionRequest(
  state: AdapterState,
  request: RequestPermissionRequest
): AcpChatUpdate[] {
  const toolCallId = request.toolCall.toolCallId;
  const existing = state.messages.some((message) =>
    message.content.some(
      (content) =>
        content.type === 'actionRequired' &&
        content.data.actionType === 'toolConfirmation' &&
        content.data.id === toolCallId
    )
  );
  if (existing) {
    return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
  }

  const identity = toolIdentity(request.toolCall);
  const prompt = permissionPrompt(request);

  state.messages.push({
    id: `acp_permission_${toolCallId}`,
    role: 'assistant',
    created: Math.floor(Date.now() / 1000),
    content: [
      {
        type: 'actionRequired',
        data: {
          actionType: 'toolConfirmation',
          id: toolCallId,
          toolName: identity.toolName ?? request.toolCall.title ?? toolCallId,
          arguments: rawInputToArguments(request.toolCall.rawInput),
          ...(prompt ? { prompt } : {}),
        },
      },
    ],
    metadata: { ...DEFAULT_VISIBLE_MESSAGE_METADATA },
  });

  return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
}

function getOrCreateAssistantMessageForUpdate(
  state: AdapterState,
  gooseMeta: GooseMessageMeta
): Message {
  const existing = findMessageForChunk(state, 'assistant', gooseMeta.messageId, gooseMeta.created);
  if (existing) {
    return existing;
  }

  const message: Message = {
    ...(gooseMeta.messageId ? { id: gooseMeta.messageId } : {}),
    role: 'assistant',
    created: gooseMeta.created ?? Math.floor(Date.now() / 1000),
    content: [],
    metadata: { ...DEFAULT_VISIBLE_MESSAGE_METADATA },
  };
  state.messages.push(message);
  return message;
}

function getOrCreateToolResponseMessageForUpdate(
  state: AdapterState,
  gooseMeta: GooseMessageMeta
): Message {
  if (gooseMeta.messageId) {
    const existing = state.messages.find(
      (message) => message.id === gooseMeta.messageId && message.role === 'user'
    );
    if (existing) {
      return existing;
    }
  }

  const message: Message = {
    ...(gooseMeta.messageId ? { id: gooseMeta.messageId } : {}),
    role: 'user',
    created: gooseMeta.created ?? Math.floor(Date.now() / 1000),
    content: [],
    metadata: { ...DEFAULT_VISIBLE_MESSAGE_METADATA },
  };
  state.messages.push(message);
  return message;
}

function applyContentChunk(
  state: AdapterState,
  role: Message['role'],
  update: Extract<
    SessionNotification['update'],
    { sessionUpdate: 'user_message_chunk' | 'agent_message_chunk' }
  >
): AcpChatUpdate[] {
  const content = messageContentFromAcpContentBlock(update.content);
  if (!content) {
    return [];
  }

  const gooseMeta = getGooseMessageMeta(update);
  const messageId = update.messageId ?? gooseMeta.messageId;
  const existing = findMessageForChunk(state, role, messageId, gooseMeta.created);

  if (existing) {
    const lastContent = existing.content[existing.content.length - 1];
    if (lastContent?.type === 'text' && content.type === 'text') {
      lastContent.text = mergeTextChunk(lastContent.text, content.text);
    } else if (content.type === 'image' && hasImageContent(existing, content)) {
      return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
    } else {
      existing.content.push(content);
    }
  } else {
    state.messages.push({
      ...(messageId ? { id: messageId } : {}),
      role,
      created: gooseMeta.created ?? Math.floor(Date.now() / 1000),
      content: [content],
      metadata: { ...DEFAULT_VISIBLE_MESSAGE_METADATA },
    });
  }

  return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
}

function rawInputToArguments(rawInput: unknown): Record<string, unknown> {
  return isRecord(rawInput) ? rawInput : {};
}

function messageWithToolResponse(state: AdapterState, toolCallId: string): Message | undefined {
  return state.messages.find((message) =>
    message.content.some((content) => content.type === 'toolResponse' && content.id === toolCallId)
  );
}

interface ToolIdentity {
  toolName?: string;
  extensionName?: string;
}

function toolIdentity(update: ToolCall | ToolCallUpdate): ToolIdentity {
  if (!isRecord(update._meta)) {
    return {};
  }

  const goose = update._meta.goose;
  if (!isRecord(goose) || !isRecord(goose.toolCall)) {
    return {};
  }

  return {
    toolName: typeof goose.toolCall.toolName === 'string' ? goose.toolCall.toolName : undefined,
    extensionName:
      typeof goose.toolCall.extensionName === 'string' ? goose.toolCall.extensionName : undefined,
  };
}

function toolRequestMetadata(
  update: ToolCall,
  identity: ToolIdentity
): Record<string, unknown> | undefined {
  const metadata: Record<string, unknown> = {};

  if (update.title) {
    metadata.title = update.title;
  }
  if (update.status) {
    metadata.status = update.status;
  }
  if (identity.extensionName) {
    metadata.extensionName = identity.extensionName;
  }
  if (update.kind) {
    metadata.kind = update.kind;
  }
  if (update.locations) {
    metadata.locations = update.locations;
  }

  return Object.keys(metadata).length > 0 ? metadata : undefined;
}

function toolResponseMetadata(
  update: ToolCallUpdate,
  identity: ToolIdentity
): Record<string, unknown> | undefined {
  const metadata: Record<string, unknown> = {};

  if (update.title) {
    metadata.title = update.title;
  }
  if (update.status) {
    metadata.status = update.status;
  }
  if (identity.extensionName) {
    metadata.extensionName = identity.extensionName;
  }
  if (update.kind) {
    metadata.kind = update.kind;
  }
  if (update.locations) {
    metadata.locations = update.locations;
  }
  if (update.rawOutput !== undefined) {
    metadata.rawOutput = update.rawOutput;
  }
  if (update.content) {
    metadata.content = update.content;
  }

  return Object.keys(metadata).length > 0 ? metadata : undefined;
}

function toolResultValue(
  update: ToolCallUpdate,
  mcpAppMeta: DesktopMcpAppMeta | undefined
): CallToolResponse {
  return {
    content: toolResultContent(update),
    isError: false,
    ...(mcpAppMeta ? { _meta: mcpAppMeta } : {}),
  };
}

function toolResultContent(update: ToolCallUpdate): ApiContentBlock[] {
  const content: ApiContentBlock[] = [];

  for (const item of update.content ?? []) {
    if (item.type !== 'content') {
      continue;
    }

    const block = apiContentBlockFromAcpContentBlock(item.content);
    if (block) {
      content.push(block);
    }
  }

  if (content.length > 0) {
    return content;
  }

  if (typeof update.rawOutput === 'string') {
    return [{ type: 'text', text: update.rawOutput }];
  }

  return [];
}

function apiContentBlockFromAcpContentBlock(content: AcpContentBlock): ApiContentBlock | undefined {
  switch (content.type) {
    case 'text':
      return {
        type: 'text',
        text: content.text,
        ...(content._meta ? { _meta: content._meta } : {}),
      };
    case 'image':
      return {
        type: 'image',
        data: content.data,
        mimeType: content.mimeType,
        ...(content._meta ? { _meta: content._meta } : {}),
      };
    case 'audio':
      return {
        type: 'audio',
        data: content.data,
        mimeType: content.mimeType,
      };
    case 'resource_link':
      return {
        type: 'resource_link',
        uri: content.uri,
        name: content.name,
        ...(content.description ? { description: content.description } : {}),
        ...(content.mimeType ? { mimeType: content.mimeType } : {}),
        ...(content.size !== undefined && content.size !== null ? { size: content.size } : {}),
        ...(content.title ? { title: content.title } : {}),
        ...(content._meta ? { _meta: content._meta } : {}),
      };
    case 'resource':
      return {
        type: 'resource',
        resource: apiResourceContentsFromAcpResource(content.resource),
        ...(content._meta ? { _meta: content._meta } : {}),
      };
    default:
      return undefined;
  }
}

function apiResourceContentsFromAcpResource(
  resource: Extract<AcpContentBlock, { type: 'resource' }>['resource']
): Extract<ApiContentBlock, { type: 'resource' }>['resource'] {
  if ('text' in resource) {
    return {
      uri: resource.uri,
      text: resource.text,
      ...(resource.mimeType ? { mimeType: resource.mimeType } : {}),
      ...(resource._meta ? { _meta: resource._meta } : {}),
    };
  }

  return {
    uri: resource.uri,
    blob: resource.blob,
    ...(resource.mimeType ? { mimeType: resource.mimeType } : {}),
    ...(resource._meta ? { _meta: resource._meta } : {}),
  };
}

function toolError(update: ToolCallUpdate): string {
  if (typeof update.rawOutput === 'string') {
    return update.rawOutput;
  }

  return update.title ?? 'Tool call failed';
}

interface DesktopMcpAppMeta extends Record<string, unknown> {
  ui: {
    resourceUri: string;
  };
  extensionName?: string;
  toolName?: string;
}

function mcpAppMetadata(update: ToolCallUpdate): DesktopMcpAppMeta | undefined {
  if (!isRecord(update._meta)) {
    return undefined;
  }

  const goose = update._meta.goose;
  if (!isRecord(goose) || !isRecord(goose.mcpApp)) {
    return undefined;
  }

  const resourceUri = goose.mcpApp.resourceUri;
  if (typeof resourceUri !== 'string') {
    return undefined;
  }

  return {
    ui: {
      resourceUri,
    },
    extensionName:
      typeof goose.mcpApp.extensionName === 'string' ? goose.mcpApp.extensionName : undefined,
    toolName: typeof goose.mcpApp.toolName === 'string' ? goose.mcpApp.toolName : undefined,
  };
}

function permissionPrompt(request: RequestPermissionRequest): string | undefined {
  for (const content of request.toolCall.content ?? []) {
    if (content.type === 'content' && content.content.type === 'text') {
      return content.content.text;
    }
  }

  return undefined;
}

function applyThoughtChunk(
  state: AdapterState,
  update: Extract<SessionNotification['update'], { sessionUpdate: 'agent_thought_chunk' }>
): AcpChatUpdate[] {
  if (update.content.type !== 'text') {
    return [];
  }

  const gooseMeta = getGooseMessageMeta(update);
  const messageId = update.messageId ?? gooseMeta.messageId;
  const existing = findMessageForChunk(state, 'assistant', messageId, gooseMeta.created);

  if (existing) {
    const lastContent = existing.content[existing.content.length - 1];
    if (lastContent?.type === 'thinking') {
      lastContent.thinking += update.content.text;
    } else {
      existing.content.push({ type: 'thinking', thinking: update.content.text, signature: '' });
    }
  } else {
    state.messages.push({
      ...(messageId ? { id: messageId } : {}),
      role: 'assistant',
      created: gooseMeta.created ?? Math.floor(Date.now() / 1000),
      content: [{ type: 'thinking', thinking: update.content.text, signature: '' }],
      metadata: { ...DEFAULT_VISIBLE_MESSAGE_METADATA },
    });
  }

  return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
}

function messageContentFromAcpContentBlock(content: AcpContentBlock): MessageContent | undefined {
  switch (content.type) {
    case 'text':
      return {
        type: 'text',
        text: content.text,
        ...(content._meta ? { _meta: content._meta } : {}),
        ...(content.annotations ? { annotations: content.annotations } : {}),
      };
    case 'image':
      return {
        type: 'image',
        data: content.data,
        mimeType: content.mimeType,
        ...(content._meta ? { _meta: content._meta } : {}),
        ...(content.annotations ? { annotations: content.annotations } : {}),
      };
    default:
      return undefined;
  }
}

function getGooseMessageMeta(update: { _meta?: unknown }): GooseMessageMeta {
  if (!isRecord(update._meta)) {
    return {};
  }

  const goose = update._meta.goose;
  if (!isRecord(goose)) {
    return {};
  }

  return {
    created: typeof goose.created === 'number' ? goose.created : undefined,
    messageId: typeof goose.messageId === 'string' ? goose.messageId : undefined,
  };
}

function findMessageForChunk(
  state: AdapterState,
  role: Message['role'],
  messageId: string | undefined,
  created: number | undefined
): Message | undefined {
  if (!messageId) {
    return lastMergeableMessageWithRole(state, role);
  }

  const existing = state.messages.find(
    (message) => message.id === messageId && message.role === role
  );
  if (existing) {
    return existing;
  }

  const pending = lastMergeableMessageWithRole(state, role);
  if (pending && !pending.id) {
    pending.id = messageId;
    pending.created = created ?? pending.created;
    return pending;
  }

  return undefined;
}

function lastMergeableMessageWithRole(
  state: AdapterState,
  role: Message['role']
): Message | undefined {
  const lastMessage = state.messages[state.messages.length - 1];
  if (lastMessage?.role !== role || lastMessage.metadata.agentVisible === false) {
    return undefined;
  }
  return lastMessage;
}

function mergeTextChunk(existing: string, incoming: string): string {
  if (!incoming || incoming === existing || existing.endsWith(incoming)) {
    return existing;
  }

  if (!existing || incoming.startsWith(existing)) {
    return incoming;
  }

  return existing + incoming;
}

function hasImageContent(message: Message, image: Extract<MessageContent, { type: 'image' }>) {
  return message.content.some(
    (content) =>
      content.type === 'image' && content.data === image.data && content.mimeType === image.mimeType
  );
}

function cloneMessage(message: Message): Message {
  return {
    ...message,
    content: message.content.map((content) => ({ ...content })),
    metadata: { ...message.metadata },
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}
