import type { GraphStatus } from '@/shared/types'

export interface KnowledgeTopologyEntity {
  entityId: string
  key?: string
  '\u0063anonicalLabel'?: string
  entityType?: string
  entitySubType?: string
  summary?: string | null
  supportCount?: number
  confidence?: number
  entityState?: string
  aliases?: string[]
}

export interface KnowledgeTopologyRelation {
  subjectEntityId: string
  objectEntityId: string
  predicate?: string
  supportCount?: number
}

export interface KnowledgeTopologyDocument {
  documentId: string
  title?: string
  fileName?: string
  external_key?: string
}

export interface KnowledgeTopologyDocumentLink {
  documentId: string
  targetNodeId: string
  supportCount?: number
}

export interface KnowledgeGraphTopologyResponse {
  documents: KnowledgeTopologyDocument[]
  entities: KnowledgeTopologyEntity[]
  relations: KnowledgeTopologyRelation[]
  documentLinks: KnowledgeTopologyDocumentLink[]
  status?: GraphStatus
  convergenceStatus?: string
  updatedAt?: string
}
