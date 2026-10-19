"""IronRAG connector framework.

Public surface
==============

To write a new connector you implement :class:`SourceAdapter` and hand it to
:func:`build_app`. The framework owns: the IronRAG HTTP client, the routing
layer (YAML), the persistent cursor state (SQLite), the push policies
(create/update/delete and duplicate handling per item kind), the periodic
sync loop, the orphan reaper, the manual-trigger and webhook FastAPI
endpoints, the single-instance pidfile lock, and structured logging.
"""

from __future__ import annotations

from .config import BaseConnectorSettings, PolicyDefaults, RunMode
from .ironrag import (
    DocumentPage,
    DocumentResource,
    IronRagCatalogError,
    IronRagClient,
    IronRagConflictError,
    IronRagContentUnsupportedError,
    IronRagContentUnsupportedProblemError,
    IronRagDuplicateContentError,
    IronRagError,
    IronRagMutationTimeoutError,
    IronRagNotFoundError,
    IronRagOperationFailedError,
    IronRagProblemError,
    OperationFailureDisposition,
    OperationHandle,
    OperationProgress,
    OperationStatus,
    OperationStatusValue,
    ProblemDetails,
)
from .observability import configure_logging, get_logger
from .orchestrator import OrchestrationFailedError, OrchestrationOutcome, Orchestrator
from .policy import DeleteAction, PushPolicy, UpdateAction, UpsertAction
from .routing import (
    PolicyOverrides,
    ResolvedLibraryTarget,
    ResolvedRoute,
    Router,
    RoutingConfig,
    RoutingError,
    RoutingReloader,
    UnroutedError,
    load_routing_config,
)
from .seed import seed_cursor
from .server import build_app
from .source import (
    IncompleteCensusError,
    SourceAdapter,
    SourceDeleteEnforcement,
    SourceExclusion,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceFetchResult,
    SourceItem,
    SourceItemRef,
    SourceMaterializationEnforcement,
    dependents_token_from,
)
from .state import StateStore
from .sync import SyncAlreadyRunningError, SyncManager, SyncReport

__all__ = [
    "BaseConnectorSettings",
    "DeleteAction",
    "DocumentPage",
    "DocumentResource",
    "IncompleteCensusError",
    "IronRagCatalogError",
    "IronRagClient",
    "IronRagConflictError",
    "IronRagContentUnsupportedError",
    "IronRagContentUnsupportedProblemError",
    "IronRagDuplicateContentError",
    "IronRagError",
    "IronRagMutationTimeoutError",
    "IronRagNotFoundError",
    "IronRagOperationFailedError",
    "IronRagProblemError",
    "OperationFailureDisposition",
    "OperationHandle",
    "OperationProgress",
    "OperationStatus",
    "OperationStatusValue",
    "OrchestrationFailedError",
    "OrchestrationOutcome",
    "Orchestrator",
    "PolicyDefaults",
    "PolicyOverrides",
    "ProblemDetails",
    "PushPolicy",
    "ResolvedLibraryTarget",
    "ResolvedRoute",
    "Router",
    "RoutingConfig",
    "RoutingError",
    "RoutingReloader",
    "RunMode",
    "SourceAdapter",
    "SourceDeleteEnforcement",
    "SourceExclusion",
    "SourceFetchDeferred",
    "SourceFetchDelete",
    "SourceFetchMaterialized",
    "SourceFetchResult",
    "SourceItem",
    "SourceItemRef",
    "SourceMaterializationEnforcement",
    "StateStore",
    "SyncAlreadyRunningError",
    "SyncManager",
    "SyncReport",
    "UnroutedError",
    "UpdateAction",
    "UpsertAction",
    "build_app",
    "configure_logging",
    "dependents_token_from",
    "get_logger",
    "load_routing_config",
    "seed_cursor",
]
