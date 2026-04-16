#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CanonicalTarget {
    VectorDatabase,
    LargeLanguageModel,
    RetrievalAugmentedGeneration,
    RustProgrammingLanguage,
    SemanticWeb,
    KnowledgeGraph,
    GraphDatabase,
}

impl CanonicalTarget {
    pub(crate) const fn subject_label(self) -> &'static str {
        match self {
            Self::VectorDatabase => "Vector database",
            Self::LargeLanguageModel => "Large language model",
            Self::RetrievalAugmentedGeneration => "Retrieval-augmented generation",
            Self::RustProgrammingLanguage => "Rust",
            Self::SemanticWeb => "Semantic web",
            Self::KnowledgeGraph => "Knowledge graph",
            Self::GraphDatabase => "Graph database",
        }
    }

    pub(crate) const fn query_aliases(self) -> &'static [&'static str] {
        match self {
            Self::VectorDatabase => &["vector database", "embeddings semantic similarity"],
            Self::LargeLanguageModel => &["large language model", "language generation reasoning"],
            Self::RetrievalAugmentedGeneration => {
                &["retrieval-augmented generation", "external documents before answering"]
            }
            Self::RustProgrammingLanguage => &["rust programming language", "memory safety"],
            Self::SemanticWeb => &["semantic web", "rdf owl machine-readable"],
            Self::KnowledgeGraph => &["knowledge graph", "interlinked descriptions entities"],
            Self::GraphDatabase => &["graph database", "gremlin sparql cypher gql"],
        }
    }

    pub(crate) fn matches_subject_label(self, subject_label: &str) -> bool {
        subject_label.trim().eq_ignore_ascii_case(self.subject_label())
    }

    pub(crate) fn corpus_mentions(self, corpus_text: &str) -> bool {
        let lowered = corpus_text.to_lowercase();
        match self {
            Self::VectorDatabase => {
                lowered.contains("vector database") || lowered.contains("vector_database")
            }
            Self::LargeLanguageModel => {
                lowered.contains("large language model") || lowered.contains("large_language_model")
            }
            Self::RetrievalAugmentedGeneration => {
                lowered.contains("retrieval augmented generation")
                    || lowered.contains("retrieval-augmented generation")
                    || lowered.contains("retrieval_augmented_generation")
                    || lowered.contains(" rag ")
            }
            Self::RustProgrammingLanguage => {
                lowered.contains("rust programming language")
                    || lowered.contains("rust_programming_language")
            }
            Self::SemanticWeb => {
                lowered.contains("semantic web") || lowered.contains("semantic_web")
            }
            Self::KnowledgeGraph => {
                lowered.contains("knowledge graph") || lowered.contains("knowledge_graph")
            }
            Self::GraphDatabase => {
                lowered.contains("graph database") || lowered.contains("graph_database")
            }
        }
    }
}
