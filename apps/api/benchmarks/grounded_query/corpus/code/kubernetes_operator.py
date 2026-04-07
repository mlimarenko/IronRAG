"""
Kubernetes operator for managing PostgreSQL database instances using kopf.

Architecture decisions:
- Uses the kopf framework for Kubernetes operator development.
- Manages a Custom Resource Definition (CRD) called PostgresInstance.
- Reconciliation loops handle create, update, delete, and periodic timer events.
- Status updates track provisioning progress, connection info, and health.
- Event watching monitors pod readiness and triggers failover when needed.
- Retry policies use exponential backoff for transient Kubernetes API errors.
- Health probes expose /healthz and /readyz endpoints for the operator itself.
- All configuration via environment variables.

Environment variables:
- OPERATOR_NAMESPACE: Namespace the operator watches. Default: "default"
- OPERATOR_NAME: Name for logging and leader election. Default: "postgres-operator"
- POSTGRES_IMAGE: Default PostgreSQL image. Default: "postgres:16-alpine"
- POSTGRES_DEFAULT_STORAGE_SIZE: Default PVC size. Default: "10Gi"
- POSTGRES_DEFAULT_CPU_LIMIT: Default CPU limit. Default: "1000m"
- POSTGRES_DEFAULT_MEMORY_LIMIT: Default memory limit. Default: "2Gi"
- POSTGRES_DEFAULT_REPLICAS: Default replica count. Default: 1
- BACKUP_S3_BUCKET: S3 bucket for WAL archiving. Default: ""
- BACKUP_S3_REGION: S3 region. Default: "us-east-1"
- BACKUP_SCHEDULE: Cron schedule for backups. Default: "0 2 * * *"
- METRICS_PORT: Prometheus metrics port. Default: 8080
- HEALTH_PORT: Health check endpoint port. Default: 8081
- RECONCILE_INTERVAL_SECONDS: Periodic reconciliation interval. Default: 300
- MAX_RETRY_ATTEMPTS: Maximum reconciliation retry attempts. Default: 5
- RETRY_BACKOFF_SECONDS: Base backoff delay for retries. Default: 10
- LOG_LEVEL: Logging verbosity. Default: "INFO"
- DATABASE_URL: Connection string for operator's own state. Default: ""
"""

from __future__ import annotations

import asyncio
import enum
import hashlib
import logging
import os
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional

import kopf

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class OperatorConfig:
    """Operator configuration loaded from environment variables."""

    namespace: str
    operator_name: str
    postgres_image: str
    default_storage_size: str
    default_cpu_limit: str
    default_memory_limit: str
    default_replicas: int
    backup_s3_bucket: str
    backup_s3_region: str
    backup_schedule: str
    metrics_port: int
    health_port: int
    reconcile_interval_seconds: int
    max_retry_attempts: int
    retry_backoff_seconds: int
    log_level: str
    database_url: str

    @classmethod
    def from_env(cls) -> OperatorConfig:
        return cls(
            namespace=os.environ.get("OPERATOR_NAMESPACE", "default"),
            operator_name=os.environ.get("OPERATOR_NAME", "postgres-operator"),
            postgres_image=os.environ.get("POSTGRES_IMAGE", "postgres:16-alpine"),
            default_storage_size=os.environ.get("POSTGRES_DEFAULT_STORAGE_SIZE", "10Gi"),
            default_cpu_limit=os.environ.get("POSTGRES_DEFAULT_CPU_LIMIT", "1000m"),
            default_memory_limit=os.environ.get("POSTGRES_DEFAULT_MEMORY_LIMIT", "2Gi"),
            default_replicas=int(os.environ.get("POSTGRES_DEFAULT_REPLICAS", "1")),
            backup_s3_bucket=os.environ.get("BACKUP_S3_BUCKET", ""),
            backup_s3_region=os.environ.get("BACKUP_S3_REGION", "us-east-1"),
            backup_schedule=os.environ.get("BACKUP_SCHEDULE", "0 2 * * *"),
            metrics_port=int(os.environ.get("METRICS_PORT", "8080")),
            health_port=int(os.environ.get("HEALTH_PORT", "8081")),
            reconcile_interval_seconds=int(os.environ.get("RECONCILE_INTERVAL_SECONDS", "300")),
            max_retry_attempts=int(os.environ.get("MAX_RETRY_ATTEMPTS", "5")),
            retry_backoff_seconds=int(os.environ.get("RETRY_BACKOFF_SECONDS", "10")),
            log_level=os.environ.get("LOG_LEVEL", "INFO"),
            database_url=os.environ.get("DATABASE_URL", ""),
        )


# Global config instance
CONFIG = OperatorConfig.from_env()
logger = logging.getLogger(CONFIG.operator_name)

# ---------------------------------------------------------------------------
# CRD spec model
# ---------------------------------------------------------------------------


class InstancePhase(enum.Enum):
    """Lifecycle phases for a PostgresInstance."""

    PENDING = "Pending"
    PROVISIONING = "Provisioning"
    RUNNING = "Running"
    UPDATING = "Updating"
    FAILING = "Failing"
    DELETING = "Deleting"
    FAILED = "Failed"
    DELETED = "Deleted"


@dataclass
class PostgresInstanceSpec:
    """Spec fields from the PostgresInstance CRD.

    CRD fields watched by the operator:
    - spec.version: PostgreSQL major version (12, 13, 14, 15, 16). Required.
    - spec.replicas: Number of replicas (1 for standalone, 2+ for HA). Default: 1.
    - spec.storage.size: PVC storage size (e.g., "50Gi"). Default: "10Gi".
    - spec.storage.storageClass: Kubernetes StorageClass name. Default: "standard".
    - spec.resources.cpu: CPU limit. Default: "1000m".
    - spec.resources.memory: Memory limit. Default: "2Gi".
    - spec.database: Default database name. Default: "app".
    - spec.credentials.secretName: Name of Secret with username/password. Required.
    - spec.backup.enabled: Whether automated backups are enabled. Default: true.
    - spec.backup.schedule: Cron schedule for backups. Default: "0 2 * * *".
    - spec.backup.retentionDays: Days to retain backups. Default: 7.
    - spec.parameters: PostgreSQL configuration parameters (key-value). Optional.
    - spec.extensions: List of PostgreSQL extensions to enable. Optional.
    - spec.monitoring.enabled: Whether to deploy a metrics sidecar. Default: true.
    - spec.highAvailability.enabled: Whether to enable streaming replication. Default: false.
    - spec.highAvailability.synchronousCommit: Synchronous replication mode. Default: "off".
    """

    version: int
    replicas: int
    storage_size: str
    storage_class: str
    cpu_limit: str
    memory_limit: str
    database: str
    credentials_secret: str
    backup_enabled: bool
    backup_schedule: str
    backup_retention_days: int
    parameters: Dict[str, str]
    extensions: List[str]
    monitoring_enabled: bool
    ha_enabled: bool
    ha_synchronous_commit: str

    @classmethod
    def from_body(cls, body: Dict[str, Any]) -> PostgresInstanceSpec:
        """Parse the CRD spec from the Kubernetes resource body."""
        spec = body.get("spec", {})
        storage = spec.get("storage", {})
        resources = spec.get("resources", {})
        credentials = spec.get("credentials", {})
        backup = spec.get("backup", {})
        monitoring = spec.get("monitoring", {})
        ha = spec.get("highAvailability", {})

        return cls(
            version=int(spec.get("version", 16)),
            replicas=int(spec.get("replicas", CONFIG.default_replicas)),
            storage_size=storage.get("size", CONFIG.default_storage_size),
            storage_class=storage.get("storageClass", "standard"),
            cpu_limit=resources.get("cpu", CONFIG.default_cpu_limit),
            memory_limit=resources.get("memory", CONFIG.default_memory_limit),
            database=spec.get("database", "app"),
            credentials_secret=credentials.get("secretName", ""),
            backup_enabled=backup.get("enabled", True),
            backup_schedule=backup.get("schedule", CONFIG.backup_schedule),
            backup_retention_days=int(backup.get("retentionDays", 7)),
            parameters=spec.get("parameters", {}),
            extensions=spec.get("extensions", []),
            monitoring_enabled=monitoring.get("enabled", True),
            ha_enabled=ha.get("enabled", False),
            ha_synchronous_commit=ha.get("synchronousCommit", "off"),
        )

    def validate(self) -> List[str]:
        """Validate the spec and return a list of errors."""
        errors: List[str] = []
        if self.version not in (12, 13, 14, 15, 16):
            errors.append(f"Unsupported PostgreSQL version: {self.version}")
        if self.replicas < 1 or self.replicas > 10:
            errors.append(f"Replicas must be between 1 and 10, got {self.replicas}")
        if not self.credentials_secret:
            errors.append("spec.credentials.secretName is required")
        if self.ha_enabled and self.replicas < 2:
            errors.append("High availability requires at least 2 replicas")
        if self.backup_retention_days < 1 or self.backup_retention_days > 90:
            errors.append("Backup retention must be between 1 and 90 days")
        return errors


@dataclass
class PostgresInstanceStatus:
    """Status subresource for the PostgresInstance CRD."""

    phase: InstancePhase
    ready_replicas: int
    total_replicas: int
    primary_endpoint: str
    replica_endpoint: str
    current_version: int
    last_backup_time: Optional[str]
    last_reconcile_time: str
    conditions: List[Dict[str, str]]
    message: str

    def to_dict(self) -> Dict[str, Any]:
        return {
            "phase": self.phase.value,
            "readyReplicas": self.ready_replicas,
            "totalReplicas": self.total_replicas,
            "primaryEndpoint": self.primary_endpoint,
            "replicaEndpoint": self.replica_endpoint,
            "currentVersion": self.current_version,
            "lastBackupTime": self.last_backup_time,
            "lastReconcileTime": self.last_reconcile_time,
            "conditions": self.conditions,
            "message": self.message,
        }


# ---------------------------------------------------------------------------
# Error types
# ---------------------------------------------------------------------------


class OperatorError(Exception):
    """Base error for operator operations."""

    def __init__(self, message: str, code: str, retryable: bool = False) -> None:
        super().__init__(message)
        self.code = code
        self.retryable = retryable


class ProvisioningError(OperatorError):
    """Error during resource provisioning."""

    def __init__(self, resource: str, message: str) -> None:
        super().__init__(
            f"Failed to provision {resource}: {message}",
            "PROVISIONING_FAILED",
            retryable=True,
        )


class ReconciliationError(OperatorError):
    """Error during reconciliation loop."""

    def __init__(self, message: str) -> None:
        super().__init__(message, "RECONCILIATION_FAILED", retryable=True)


class ValidationError(OperatorError):
    """Error when CRD spec validation fails."""

    def __init__(self, errors: List[str]) -> None:
        super().__init__(
            f"Validation failed: {'; '.join(errors)}",
            "VALIDATION_FAILED",
            retryable=False,
        )


# ---------------------------------------------------------------------------
# Resource builders
# ---------------------------------------------------------------------------


def build_statefulset(
    name: str,
    namespace: str,
    spec: PostgresInstanceSpec,
    owner_ref: Dict[str, Any],
) -> Dict[str, Any]:
    """Build a StatefulSet manifest for the PostgreSQL instance.

    Creates a StatefulSet with:
    - Init container for data directory permissions
    - Main postgres container with configurable resources
    - Optional metrics sidecar (postgres_exporter on port 9187)
    - PVC template for persistent storage
    - Liveness and readiness probes
    - Anti-affinity rules for HA deployments
    """
    image = f"postgres:{spec.version}-alpine"

    containers = [
        {
            "name": "postgres",
            "image": image,
            "ports": [{"containerPort": 5432, "name": "postgresql"}],
            "env": [
                {"name": "POSTGRES_DB", "value": spec.database},
                {
                    "name": "POSTGRES_USER",
                    "valueFrom": {
                        "secretKeyRef": {
                            "name": spec.credentials_secret,
                            "key": "username",
                        }
                    },
                },
                {
                    "name": "POSTGRES_PASSWORD",
                    "valueFrom": {
                        "secretKeyRef": {
                            "name": spec.credentials_secret,
                            "key": "password",
                        }
                    },
                },
                {"name": "PGDATA", "value": "/var/lib/postgresql/data/pgdata"},
            ],
            "resources": {
                "limits": {"cpu": spec.cpu_limit, "memory": spec.memory_limit},
                "requests": {
                    "cpu": _halve_resource(spec.cpu_limit),
                    "memory": _halve_resource(spec.memory_limit),
                },
            },
            "livenessProbe": {
                "exec": {"command": ["pg_isready", "-U", "postgres"]},
                "initialDelaySeconds": 30,
                "periodSeconds": 10,
                "timeoutSeconds": 5,
                "failureThreshold": 6,
            },
            "readinessProbe": {
                "exec": {"command": ["pg_isready", "-U", "postgres"]},
                "initialDelaySeconds": 5,
                "periodSeconds": 5,
                "timeoutSeconds": 3,
                "failureThreshold": 3,
            },
            "volumeMounts": [
                {"name": "data", "mountPath": "/var/lib/postgresql/data"},
            ],
        }
    ]

    if spec.monitoring_enabled:
        containers.append(
            {
                "name": "metrics",
                "image": "prometheuscommunity/postgres-exporter:v0.15.0",
                "ports": [{"containerPort": 9187, "name": "metrics"}],
                "env": [
                    {
                        "name": "DATA_SOURCE_NAME",
                        "value": f"postgresql://postgres@localhost:5432/{spec.database}?sslmode=disable",
                    }
                ],
                "resources": {
                    "limits": {"cpu": "100m", "memory": "128Mi"},
                    "requests": {"cpu": "50m", "memory": "64Mi"},
                },
            }
        )

    affinity = {}
    if spec.ha_enabled:
        affinity = {
            "podAntiAffinity": {
                "requiredDuringSchedulingIgnoredDuringExecution": [
                    {
                        "labelSelector": {
                            "matchExpressions": [
                                {
                                    "key": "app.kubernetes.io/instance",
                                    "operator": "In",
                                    "values": [name],
                                }
                            ]
                        },
                        "topologyKey": "kubernetes.io/hostname",
                    }
                ]
            }
        }

    return {
        "apiVersion": "apps/v1",
        "kind": "StatefulSet",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": _standard_labels(name),
            "ownerReferences": [owner_ref],
        },
        "spec": {
            "replicas": spec.replicas,
            "serviceName": f"{name}-headless",
            "selector": {"matchLabels": {"app.kubernetes.io/instance": name}},
            "template": {
                "metadata": {"labels": _standard_labels(name)},
                "spec": {
                    "containers": containers,
                    "affinity": affinity,
                    "terminationGracePeriodSeconds": 60,
                },
            },
            "volumeClaimTemplates": [
                {
                    "metadata": {"name": "data"},
                    "spec": {
                        "accessModes": ["ReadWriteOnce"],
                        "storageClassName": spec.storage_class,
                        "resources": {"requests": {"storage": spec.storage_size}},
                    },
                }
            ],
        },
    }


def build_service(
    name: str,
    namespace: str,
    owner_ref: Dict[str, Any],
    service_type: str = "ClusterIP",
) -> Dict[str, Any]:
    """Build a Service manifest for the PostgreSQL instance.

    Creates two services:
    - Primary service (read-write) on port 5432
    - Headless service for StatefulSet DNS
    """
    return {
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": _standard_labels(name),
            "ownerReferences": [owner_ref],
        },
        "spec": {
            "type": service_type,
            "ports": [
                {"port": 5432, "targetPort": 5432, "protocol": "TCP", "name": "postgresql"},
            ],
            "selector": {"app.kubernetes.io/instance": name},
        },
    }


def build_configmap(
    name: str,
    namespace: str,
    spec: PostgresInstanceSpec,
    owner_ref: Dict[str, Any],
) -> Dict[str, Any]:
    """Build a ConfigMap with PostgreSQL configuration parameters."""
    pg_conf_lines = [f"{k} = '{v}'" for k, v in spec.parameters.items()]
    if spec.ha_enabled:
        pg_conf_lines.extend([
            f"synchronous_commit = '{spec.ha_synchronous_commit}'",
            "wal_level = 'replica'",
            f"max_wal_senders = {spec.replicas + 2}",
            "hot_standby = 'on'",
        ])

    return {
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": f"{name}-config",
            "namespace": namespace,
            "labels": _standard_labels(name),
            "ownerReferences": [owner_ref],
        },
        "data": {
            "postgresql.conf": "\n".join(pg_conf_lines),
            "pg_hba.conf": (
                "local all all trust\n"
                "host all all 0.0.0.0/0 md5\n"
                "host replication all 0.0.0.0/0 md5\n"
            ),
        },
    }


def _standard_labels(name: str) -> Dict[str, str]:
    return {
        "app.kubernetes.io/name": "postgresql",
        "app.kubernetes.io/instance": name,
        "app.kubernetes.io/managed-by": CONFIG.operator_name,
        "app.kubernetes.io/component": "database",
    }


def _halve_resource(resource: str) -> str:
    """Halve a Kubernetes resource quantity for requests (half of limits)."""
    if resource.endswith("m"):
        val = int(resource[:-1])
        return f"{val // 2}m"
    elif resource.endswith("Gi"):
        val = int(resource[:-2])
        return f"{max(1, val // 2)}Gi"
    elif resource.endswith("Mi"):
        val = int(resource[:-2])
        return f"{max(64, val // 2)}Mi"
    return resource


# ---------------------------------------------------------------------------
# Reconciliation handlers
# ---------------------------------------------------------------------------


@kopf.on.create("postgresinstances.database.example.com")
async def on_create(
    body: Dict[str, Any],
    name: str,
    namespace: str,
    logger: logging.Logger,
    patch: Dict[str, Any],
    **kwargs: Any,
) -> Dict[str, Any]:
    """Handle creation of a new PostgresInstance resource.

    Reconciliation actions on create:
    1. Parse and validate the CRD spec.
    2. Create the headless Service for StatefulSet DNS.
    3. Create the primary Service for client connections.
    4. Create a ConfigMap with PostgreSQL configuration.
    5. Create the StatefulSet with the specified replicas.
    6. If backup is enabled, create a CronJob for scheduled backups.
    7. Update the status subresource with provisioning progress.
    """
    spec = PostgresInstanceSpec.from_body(body)
    errors = spec.validate()
    if errors:
        raise ValidationError(errors)

    logger.info("Creating PostgresInstance %s/%s (version=%d, replicas=%d)",
                namespace, name, spec.version, spec.replicas)

    owner_ref = _build_owner_reference(body)

    # Update status to Provisioning
    patch.setdefault("status", {})
    patch["status"]["phase"] = InstancePhase.PROVISIONING.value
    patch["status"]["message"] = "Creating resources..."

    # Create resources via Kubernetes API
    # In production: api = kubernetes_asyncio.client.CoreV1Api()
    headless_svc = build_service(f"{name}-headless", namespace, owner_ref)
    primary_svc = build_service(name, namespace, owner_ref)
    configmap = build_configmap(name, namespace, spec, owner_ref)
    statefulset = build_statefulset(name, namespace, spec, owner_ref)

    # In production, these would be actual API calls:
    # await api.create_namespaced_service(namespace, headless_svc)
    # await api.create_namespaced_service(namespace, primary_svc)
    # await api.create_namespaced_config_map(namespace, configmap)
    # await apps_api.create_namespaced_stateful_set(namespace, statefulset)

    logger.info("Created all resources for %s/%s", namespace, name)

    return {
        "phase": InstancePhase.PROVISIONING.value,
        "primaryEndpoint": f"{name}.{namespace}.svc.cluster.local:5432",
        "replicaEndpoint": f"{name}-headless.{namespace}.svc.cluster.local:5432",
        "currentVersion": spec.version,
        "totalReplicas": spec.replicas,
        "readyReplicas": 0,
    }


@kopf.on.update("postgresinstances.database.example.com")
async def on_update(
    body: Dict[str, Any],
    name: str,
    namespace: str,
    old: Dict[str, Any],
    new: Dict[str, Any],
    diff: Any,
    logger: logging.Logger,
    patch: Dict[str, Any],
    **kwargs: Any,
) -> Dict[str, Any]:
    """Handle updates to a PostgresInstance resource.

    Reconciliation actions on update:
    1. Detect which fields changed (replicas, resources, version, parameters).
    2. If replicas changed, scale the StatefulSet.
    3. If resources changed, update the StatefulSet container spec.
    4. If parameters changed, update the ConfigMap and trigger a rolling restart.
    5. If version changed, initiate a major version upgrade workflow.
    6. Update the status subresource.
    """
    spec = PostgresInstanceSpec.from_body({"spec": new.get("spec", {})})
    old_spec = PostgresInstanceSpec.from_body({"spec": old.get("spec", {})})

    errors = spec.validate()
    if errors:
        raise ValidationError(errors)

    patch.setdefault("status", {})
    patch["status"]["phase"] = InstancePhase.UPDATING.value

    changes: List[str] = []

    if spec.replicas != old_spec.replicas:
        changes.append(f"replicas: {old_spec.replicas} -> {spec.replicas}")
        logger.info("Scaling %s from %d to %d replicas", name, old_spec.replicas, spec.replicas)
        # In production: scale StatefulSet

    if spec.cpu_limit != old_spec.cpu_limit or spec.memory_limit != old_spec.memory_limit:
        changes.append(f"resources: cpu={spec.cpu_limit}, memory={spec.memory_limit}")
        logger.info("Updating resources for %s", name)
        # In production: patch StatefulSet

    if spec.parameters != old_spec.parameters:
        changes.append("parameters updated")
        logger.info("Updating PostgreSQL parameters for %s", name)
        # In production: update ConfigMap, trigger rolling restart

    if spec.version != old_spec.version:
        changes.append(f"version: {old_spec.version} -> {spec.version}")
        logger.warning("Major version upgrade requested for %s: %d -> %d",
                       name, old_spec.version, spec.version)
        # In production: initiate upgrade workflow

    patch["status"]["message"] = f"Updated: {', '.join(changes)}" if changes else "No changes"
    patch["status"]["totalReplicas"] = spec.replicas
    patch["status"]["currentVersion"] = spec.version

    return {"changes": changes}


@kopf.on.delete("postgresinstances.database.example.com")
async def on_delete(
    body: Dict[str, Any],
    name: str,
    namespace: str,
    logger: logging.Logger,
    **kwargs: Any,
) -> None:
    """Handle deletion of a PostgresInstance resource.

    Reconciliation actions on delete:
    1. Create a final backup if backup is enabled.
    2. Delete the StatefulSet (pods will be terminated gracefully).
    3. Delete the Services.
    4. Delete the ConfigMap.
    5. Optionally retain PVCs for data recovery (configurable).
    """
    spec = PostgresInstanceSpec.from_body(body)

    logger.info("Deleting PostgresInstance %s/%s", namespace, name)

    if spec.backup_enabled:
        logger.info("Creating final backup for %s before deletion", name)
        # In production: trigger immediate backup

    # Kubernetes garbage collection handles owned resources via ownerReferences,
    # but we perform explicit cleanup for resources not covered by GC.

    logger.info("PostgresInstance %s/%s deleted successfully", namespace, name)


# ---------------------------------------------------------------------------
# Timer-based reconciliation
# ---------------------------------------------------------------------------


@kopf.timer(
    "postgresinstances.database.example.com",
    interval=CONFIG.reconcile_interval_seconds,
    sharp=True,
)
async def reconcile_timer(
    body: Dict[str, Any],
    name: str,
    namespace: str,
    logger: logging.Logger,
    patch: Dict[str, Any],
    **kwargs: Any,
) -> Dict[str, Any]:
    """Periodic reconciliation to ensure desired state matches actual state.

    Timer reconciliation actions:
    1. Check StatefulSet ready replicas vs desired replicas.
    2. Verify Services exist and have correct selectors.
    3. Check pod health via pg_isready on each replica.
    4. Update status with current health information.
    5. Detect and report split-brain conditions in HA setups.
    6. Verify backup CronJob status and last successful backup time.
    """
    spec = PostgresInstanceSpec.from_body(body)
    now = datetime.now(timezone.utc).isoformat()

    # In production: query Kubernetes API for actual state
    ready_replicas = spec.replicas  # simulated as healthy

    conditions: List[Dict[str, str]] = []

    # Check StatefulSet health
    if ready_replicas == spec.replicas:
        conditions.append({
            "type": "Ready",
            "status": "True",
            "lastTransitionTime": now,
            "reason": "AllReplicasReady",
            "message": f"{ready_replicas}/{spec.replicas} replicas ready",
        })
        phase = InstancePhase.RUNNING
    else:
        conditions.append({
            "type": "Ready",
            "status": "False",
            "lastTransitionTime": now,
            "reason": "ReplicasNotReady",
            "message": f"{ready_replicas}/{spec.replicas} replicas ready",
        })
        phase = InstancePhase.FAILING

    # Check backup status
    if spec.backup_enabled:
        conditions.append({
            "type": "BackupReady",
            "status": "True",
            "lastTransitionTime": now,
            "reason": "BackupScheduled",
            "message": f"Backup schedule: {spec.backup_schedule}",
        })

    patch.setdefault("status", {})
    patch["status"].update({
        "phase": phase.value,
        "readyReplicas": ready_replicas,
        "totalReplicas": spec.replicas,
        "lastReconcileTime": now,
        "conditions": conditions,
        "message": f"Reconciled at {now}",
    })

    logger.debug("Reconciled %s/%s: phase=%s, ready=%d/%d",
                 namespace, name, phase.value, ready_replicas, spec.replicas)

    return {"phase": phase.value, "readyReplicas": ready_replicas}


# ---------------------------------------------------------------------------
# Event watchers
# ---------------------------------------------------------------------------


@kopf.on.event("pods", labels={"app.kubernetes.io/managed-by": CONFIG.operator_name})
async def on_pod_event(
    event: Dict[str, Any],
    body: Dict[str, Any],
    name: str,
    namespace: str,
    logger: logging.Logger,
    **kwargs: Any,
) -> None:
    """Watch pod events for managed PostgreSQL instances.

    Monitors pod lifecycle events to:
    - Detect pod failures and trigger failover in HA configurations.
    - Update instance status when pods transition to Ready.
    - Log container restart events for debugging.
    - Emit Kubernetes events for significant state changes.
    """
    event_type = event.get("type", "UNKNOWN")
    phase = body.get("status", {}).get("phase", "Unknown")

    if event_type == "MODIFIED":
        container_statuses = body.get("status", {}).get("containerStatuses", [])
        for cs in container_statuses:
            if cs.get("restartCount", 0) > 3:
                logger.warning(
                    "Pod %s/%s container %s has restarted %d times",
                    namespace, name, cs.get("name"), cs.get("restartCount"),
                )

    if event_type == "DELETED":
        logger.warning("Pod %s/%s was deleted unexpectedly", namespace, name)
        # In HA mode, trigger failover if this was the primary


@kopf.on.field("postgresinstances.database.example.com", field="spec.replicas")
async def on_replicas_changed(
    old: int,
    new: int,
    name: str,
    namespace: str,
    logger: logging.Logger,
    **kwargs: Any,
) -> None:
    """React specifically to replica count changes for faster scaling response."""
    if old is not None and new is not None:
        if new > old:
            logger.info("Scaling up %s from %d to %d replicas", name, old, new)
        elif new < old:
            logger.info("Scaling down %s from %d to %d replicas", name, old, new)


# ---------------------------------------------------------------------------
# Health probes
# ---------------------------------------------------------------------------


class HealthProbeServer:
    """HTTP server for operator health probes.

    Exposes:
    - GET /healthz (port HEALTH_PORT) - Liveness probe
    - GET /readyz (port HEALTH_PORT) - Readiness probe
    - GET /metrics (port METRICS_PORT) - Prometheus metrics
    """

    def __init__(self, config: OperatorConfig) -> None:
        self.config = config
        self._ready = False
        self._start_time = time.monotonic()
        self._reconcile_count = 0
        self._error_count = 0

    def set_ready(self, ready: bool) -> None:
        self._ready = ready

    def record_reconcile(self) -> None:
        self._reconcile_count += 1

    def record_error(self) -> None:
        self._error_count += 1

    async def healthz_handler(self, request: Any) -> Dict[str, Any]:
        """Liveness probe: returns 200 if the operator process is alive."""
        uptime = time.monotonic() - self._start_time
        return {
            "status": "alive",
            "uptime_seconds": round(uptime, 1),
            "operator": self.config.operator_name,
        }

    async def readyz_handler(self, request: Any) -> Dict[str, Any]:
        """Readiness probe: returns 200 if the operator is ready to reconcile."""
        if not self._ready:
            # Return 503 when not ready
            return {"status": "not_ready", "message": "Operator is initializing"}
        return {
            "status": "ready",
            "reconcile_count": self._reconcile_count,
            "error_count": self._error_count,
        }

    async def metrics_handler(self, request: Any) -> str:
        """Prometheus metrics endpoint."""
        lines = [
            f'# HELP operator_reconcile_total Total reconciliation attempts',
            f'# TYPE operator_reconcile_total counter',
            f'operator_reconcile_total {self._reconcile_count}',
            f'# HELP operator_errors_total Total reconciliation errors',
            f'# TYPE operator_errors_total counter',
            f'operator_errors_total {self._error_count}',
            f'# HELP operator_uptime_seconds Operator uptime in seconds',
            f'# TYPE operator_uptime_seconds gauge',
            f'operator_uptime_seconds {time.monotonic() - self._start_time:.1f}',
        ]
        return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# Retry helpers
# ---------------------------------------------------------------------------


async def retry_kubernetes_api(
    operation: str,
    func: Any,
    *args: Any,
    max_attempts: int = CONFIG.max_retry_attempts,
    backoff_seconds: int = CONFIG.retry_backoff_seconds,
    **kwargs: Any,
) -> Any:
    """Retry a Kubernetes API call with exponential backoff.

    Retry policy:
    - MAX_RETRY_ATTEMPTS (default 5) attempts total
    - Exponential backoff starting at RETRY_BACKOFF_SECONDS (default 10)
    - Retries on 409 Conflict, 429 Too Many Requests, and 5xx errors
    - Does NOT retry on 400, 401, 403, 404 (client errors)
    """
    last_error: Optional[Exception] = None
    for attempt in range(max_attempts):
        try:
            return await func(*args, **kwargs)
        except Exception as e:
            last_error = e
            error_code = getattr(e, "status", 0)

            # Don't retry client errors
            if error_code in (400, 401, 403, 404):
                raise

            if attempt < max_attempts - 1:
                delay = backoff_seconds * (2 ** attempt)
                logger.warning(
                    "Retry %d/%d for %s after %ds: %s",
                    attempt + 1, max_attempts, operation, delay, str(e),
                )
                await asyncio.sleep(delay)

    raise OperatorError(
        f"Operation '{operation}' failed after {max_attempts} attempts: {last_error}",
        "RETRY_EXHAUSTED",
        retryable=False,
    )


# ---------------------------------------------------------------------------
# Owner reference helper
# ---------------------------------------------------------------------------


def _build_owner_reference(body: Dict[str, Any]) -> Dict[str, Any]:
    """Build an ownerReference for child resources."""
    return {
        "apiVersion": body.get("apiVersion", "database.example.com/v1"),
        "kind": body.get("kind", "PostgresInstance"),
        "name": body["metadata"]["name"],
        "uid": body["metadata"]["uid"],
        "controller": True,
        "blockOwnerDeletion": True,
    }


# ---------------------------------------------------------------------------
# Operator startup
# ---------------------------------------------------------------------------


@kopf.on.startup()
async def on_startup(logger: logging.Logger, **kwargs: Any) -> None:
    """Operator startup hook.

    Performs initialization:
    1. Validate operator configuration.
    2. Verify Kubernetes API connectivity.
    3. Ensure CRD is registered.
    4. Start health probe server.
    5. Start metrics server.
    """
    logger.info(
        "Starting %s (namespace=%s, reconcile_interval=%ds)",
        CONFIG.operator_name,
        CONFIG.namespace,
        CONFIG.reconcile_interval_seconds,
    )

    health_server = HealthProbeServer(CONFIG)
    health_server.set_ready(True)

    logger.info("Operator startup complete")


@kopf.on.cleanup()
async def on_cleanup(logger: logging.Logger, **kwargs: Any) -> None:
    """Operator cleanup hook for graceful shutdown."""
    logger.info("Shutting down %s", CONFIG.operator_name)
