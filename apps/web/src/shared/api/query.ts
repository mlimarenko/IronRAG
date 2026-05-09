import { Query } from "./generated";
import { unwrap } from "./runtime";
import type {
  AssistantExecutionDetail,
  AssistantHydratedConversation,
  AssistantSessionListItem,
  AssistantSystemPromptResponse,
  LlmContextSnapshot,
  QueryConversation,
} from "./generated";

export type AssistantTurnExecutionResponse = AssistantExecutionDetail;
export type LlmContextDebugResponse = LlmContextSnapshot;

export const queryApi = {
  listSessions: (params: { workspaceId: string; libraryId: string }) =>
    Query.listQuerySessions({ query: { libraryId: params.libraryId } }).then(
      (result): AssistantSessionListItem[] => unwrap(result),
    ),
  createSession: (workspaceId: string, libraryId: string) =>
    Query.createQuerySession({ body: { workspaceId, libraryId } }).then(
      (result): QueryConversation => unwrap(result),
    ),
  getSession: (sessionId: string) =>
    Query.getQuerySession({ path: { sessionId } }).then(
      (result): AssistantHydratedConversation => unwrap(result),
    ),
  createTurn: (sessionId: string, contentText: string) =>
    Query.createQuerySessionTurn({
      path: { sessionId },
      body: { contentText },
    }).then((result): AssistantTurnExecutionResponse => unwrap(result)),
  getExecution: (executionId: string) =>
    Query.getQueryExecution({ path: { executionId } }).then(
      (result): AssistantExecutionDetail => unwrap(result),
    ),
  getExecutionLlmContext: (executionId: string) =>
    Query.getQueryExecutionLlmContext({ path: { executionId } }).then(
      (result): LlmContextDebugResponse => unwrap(result),
    ),
  getAssistantSystemPrompt: (libraryId?: string) =>
    Query.getAssistantSystemPrompt({ query: { libraryId } }).then(
      (result): AssistantSystemPromptResponse => unwrap(result),
    ),
};
