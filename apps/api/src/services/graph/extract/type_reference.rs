//! The canonical entity type reference: one definition per assignable type.
//!
//! Graph extraction renders it into its prompt, and the optional entity-typing
//! stage sends the same definitions to its classifier — as SystemOne `Choice`
//! option descriptions or in the chat system prompt — so every component that
//! chooses a type reads one vocabulary (IRG-184).

use crate::{
    domains::runtime_graph::RuntimeNodeType, services::graph::identity::runtime_node_type_slug,
};

/// One assignable entity type with its definition and illustrative examples.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EntityTypeReference {
    pub(crate) node_type: RuntimeNodeType,
    pub(crate) definition: &'static str,
    /// Comma-separated examples; empty for the catch-all type.
    pub(crate) examples: &'static str,
}

/// Every type an extracted entity may take, in reference order. `Document` is
/// absent: the pipeline reserves it for the document node itself.
pub(crate) const ENTITY_TYPE_REFERENCE: [EntityTypeReference; 10] = [
    EntityTypeReference {
        node_type: RuntimeNodeType::Person,
        definition: "A named individual human being",
        examples: "Linus Torvalds, Marie Curie, Warren Buffett, Hippocrates",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Organization,
        definition: "A company, institution, government body, team, or standards body",
        examples: "Google, WHO, SEC, IETF, Supreme Court, Red Cross",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Location,
        definition: "A named geographic place, region, facility, or site",
        examples: "Silicon Valley, Wall Street, Chernobyl, Amazon rainforest",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Event,
        definition: "A named occurrence, incident, milestone, or time-bounded happening",
        examples: "COVID-19 pandemic, Log4Shell, 2008 financial crisis, Roe v. Wade, Apollo 11",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Artifact,
        definition: "Anything created, built, or designed by humans — software, tools, products, drugs, devices, standards, protocols, laws, licenses, code functions, APIs, documents",
        examples: "PostgreSQL, Aspirin, TCP/IP, GDPR, MIT License, build_router(), GET /api/users, Basel III, React, insulin pump",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Natural,
        definition: "Anything existing in nature without human creation — species, organisms, diseases, genes, proteins, elements, minerals, natural phenomena",
        examples: "SARS-CoV-2, BRCA1 gene, malaria, silicon, photosynthesis, earthquake, DNA",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Process,
        definition: "A named procedure, method, algorithm, workflow, or repeatable sequence of steps",
        examples: "Agile methodology, PCR testing, IPO process, judicial review, gradient descent, fermentation",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Concept,
        definition: "An abstract idea, theory, principle, pattern, paradigm, theme, or field of study",
        examples: "dependency injection, herd immunity, supply and demand, due process, machine learning, oncology, relativity",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Attribute,
        definition: "A named measurable property, metric, indicator, parameter, status, threshold, or configuration value",
        examples: "p99 latency, blood pressure, GDP, APP_PORT, credit score, HTTP 200, melting point, pH level",
    },
    EntityTypeReference {
        node_type: RuntimeNodeType::Entity,
        definition: "Catch-all for named things that do not fit any other type. Always prefer a more specific type above.",
        examples: "",
    },
];

impl EntityTypeReference {
    #[must_use]
    pub(crate) fn slug(&self) -> &'static str {
        runtime_node_type_slug(&self.node_type)
    }

    /// One reference line: `- <slug>: <definition> (<examples>)`.
    #[must_use]
    pub(crate) fn line(&self) -> String {
        if self.examples.is_empty() {
            format!("- {}: {}", self.slug(), self.definition)
        } else {
            format!("- {}: {} ({})", self.slug(), self.definition, self.examples)
        }
    }
}

/// The reference lines, one per type, joined by newlines.
#[must_use]
pub(crate) fn entity_type_reference_lines() -> String {
    ENTITY_TYPE_REFERENCE.iter().map(EntityTypeReference::line).collect::<Vec<_>>().join("\n")
}
