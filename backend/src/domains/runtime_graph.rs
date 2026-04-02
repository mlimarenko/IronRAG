use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeNodeType {
    Document,
    Entity,
    Topic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeGraphArtifactFilterReason {
    EmptyRelation,
    DegenerateSelfLoop,
    LowValueArtifact,
}
