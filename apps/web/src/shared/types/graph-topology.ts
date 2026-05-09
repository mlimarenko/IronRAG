import type { GraphStatus } from '@/shared/types';

export interface KnowledgeTopologyEntity {
  id?: string;
  entityId?: string;
  key?: string;
  label?: string;
  canonicalLabel?: string;
  entityType?: string;
  entitySubType?: string;
  summary?: string | null;
  supportCount?: number;
  confidence?: number;
  entityState?: string;
  aliases?: string[];
  nodeType?: string;
}

export interface KnowledgeTopologyRelation {
  id?: string;
  relationId?: string;
  subjectEntityId: string;
  objectEntityId: string;
  predicate?: string;
  supportCount?: number;
}

export interface KnowledgeTopologyDocument {
  id?: string;
  document_id?: string;
  documentId?: string;
  title?: string;
  fileName?: string;
  external_key?: string;
  document_state?: string;
}

export interface KnowledgeTopologyDocumentLink {
  documentId: string;
  targetNodeId: string;
  supportCount?: number;
}

export interface KnowledgeGraphTopologyResponse {
  documents: KnowledgeTopologyDocument[];
  entities: KnowledgeTopologyEntity[];
  relations: KnowledgeTopologyRelation[];
  documentLinks: KnowledgeTopologyDocumentLink[];
  status?: GraphStatus;
  convergenceStatus?: string;
  updatedAt?: string;
}
