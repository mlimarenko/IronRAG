//! Static dictionary of the bundled connector sources.
//!
//! `connectors/README.md` ("Running one", "Sources") is the human-facing
//! version of the same table. This module is the code-side owner of the
//! vocabulary so the MCP connector tools and any other consumer read one
//! list instead of re-declaring image names and run modes.

/// Static description of one bundled connector source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorSourceSpec {
    /// Stable connector kind, also used in routing configuration.
    pub kind: &'static str,
    /// Human-readable source name.
    pub display_name: &'static str,
    /// Run modes the connector supports: `poll`, `webhook`, or both.
    pub run_modes: &'static [&'static str],
    /// Published container image for this connector.
    pub image: &'static str,
    /// Deployment template directory under `connectors/deploy`.
    pub deploy_directory: &'static str,
}

/// The bundled connector sources in catalog order.
pub const CONNECTOR_SOURCES: &[ConnectorSourceSpec] = &[
    ConnectorSourceSpec {
        kind: "bookstack",
        display_name: "BookStack",
        run_modes: &["poll", "webhook"],
        image: "pipingspace/ironrag.bookstack",
        deploy_directory: "connectors/deploy/bookstack",
    },
    ConnectorSourceSpec {
        kind: "confluence",
        display_name: "Confluence",
        run_modes: &["poll", "webhook"],
        image: "pipingspace/ironrag.confluence",
        deploy_directory: "connectors/deploy/confluence",
    },
    ConnectorSourceSpec {
        kind: "gitrepos",
        display_name: "Git repositories",
        run_modes: &["poll"],
        image: "pipingspace/ironrag.gitrepos",
        deploy_directory: "connectors/deploy/gitrepos",
    },
    ConnectorSourceSpec {
        kind: "youtrack",
        display_name: "YouTrack",
        run_modes: &["poll"],
        image: "pipingspace/ironrag.youtrack",
        deploy_directory: "connectors/deploy/youtrack",
    },
];
