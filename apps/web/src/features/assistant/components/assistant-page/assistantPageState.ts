import type { SetStateAction } from 'react';
import type { AssistantMessage, EvidenceBundle } from '@/shared/types';
import type { AssistantTurnExecutionResponse } from '@/shared/api/query';
import { mapAssistantTurnToEvidence } from '@/features/assistant/model/assistantAdapter';

export const STARTER_PROMPT_IDS = [
  'technologies',
  'deployment',
  'security',
  'storage',
] as const;

export const EMPTY_MESSAGES: AssistantMessage[] = [];

export type RetryableAssistantTurn = {
  question: string;
  diagnosis: string;
};

export function resolveStateAction<T>(action: SetStateAction<T>, previous: T): T {
  return typeof action === 'function'
    ? (action as (previousValue: T) => T)(previous)
    : action;
}

export function isTransientNetworkReject(message: string): boolean {
  const lower = message.toLowerCase();
  return (
    lower.includes('networkerror') ||
    lower.includes('failed to fetch') ||
    lower.includes('load failed')
  );
}

export function createUserMessage(question: string, now: number): AssistantMessage {
  return {
    id: `m-${now}`,
    role: 'user',
    content: question,
    timestamp: new Date(now).toISOString(),
  };
}

export function createPendingAssistantMessage(now: number): AssistantMessage {
  return {
    id: `m-pending-${now}`,
    role: 'assistant',
    content: '',
    timestamp: new Date(now).toISOString(),
  };
}

export function createErrorAssistantMessage(content: string): AssistantMessage {
  return {
    id: `m-err-${Date.now()}`,
    role: 'assistant',
    content,
    timestamp: new Date().toISOString(),
  };
}

export function applyTurnResultToMessages(
  messages: AssistantMessage[],
  pendingId: string,
  result: AssistantTurnExecutionResponse,
  emptyAnswerText: string,
): AssistantMessage[] {
  const answerText = result.responseTurn?.contentText ?? emptyAnswerText;
  const evidence = mapAssistantTurnToEvidence(result);

  return messages.map((message) =>
    message.id === pendingId
      ? {
          id: result.responseTurn?.id ?? pendingId,
          role: 'assistant',
          content: answerText,
          timestamp: result.responseTurn?.createdAt ?? message.timestamp,
          executionId: result.responseTurn?.executionId ?? null,
          evidence,
        }
      : message,
  );
}

export function latestEvidenceFromMessages(
  messages: AssistantMessage[],
): EvidenceBundle | undefined {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message?.role === 'assistant' && message.evidence) {
      return message.evidence;
    }
  }
  return undefined;
}
