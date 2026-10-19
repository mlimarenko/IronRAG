#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHART_DIR="${ROOT_DIR}/charts/ironrag"

. "${ROOT_DIR}/scripts/minikube/common.sh"

HELM_BIN="$(resolve_bin helm "${ROOT_DIR}")"
APP_VERSION="$("${HELM_BIN}" show chart "${CHART_DIR}" | awk -F': *' '$1 == "appVersion" { print $2; exit }')"
APP_IMAGE_TAG="v${APP_VERSION}"
RENDER_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ironrag-helm-render.XXXXXX")"
trap 'rm -rf -- "${RENDER_DIR}"' EXIT
BUNDLED_RENDER="${RENDER_DIR}/bundled.yaml"
FILESYSTEM_RENDER="${RENDER_DIR}/filesystem.yaml"
EXTERNAL_RENDER="${RENDER_DIR}/external.yaml"
HARDENING_RENDER="${RENDER_DIR}/hardening.yaml"
EXTRA_INGRESS_RENDER="${RENDER_DIR}/hardening-extra-ingress.yaml"
CREDENTIAL_RENDER="${RENDER_DIR}/credential-secret.yaml"
EXISTING_SECRET_RENDER="${RENDER_DIR}/existing-runtime-secret.yaml"
OIDC_RENDER="${RENDER_DIR}/oidc-runtime.yaml"
MIGRATION_RENDER="${RENDER_DIR}/migration-contract.yaml"
S4_MIGRATION_RENDER="${RENDER_DIR}/s4-migration-contract.yaml"
ARGO_HOOK_RENDER="${RENDER_DIR}/argocd-sync-hook.yaml"

"${HELM_BIN}" lint "${CHART_DIR}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/bundled-s3.yaml" >"${BUNDLED_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/filesystem-single-node.yaml" >"${FILESYSTEM_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/external-services.yaml" >"${EXTERNAL_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set networkPolicy.enabled=true \
  --set podDisruptionBudget.enabled=true \
  --set autoscaling.enabled=true \
  --set app.databaseMaxConnections=84 >"${HARDENING_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set networkPolicy.enabled=true \
  --set 'networkPolicy.extraIngress.api[0].from[0].namespaceSelector.matchLabels.access=clients' \
  --set 'networkPolicy.extraIngress.api[0].ports[0].protocol=TCP' \
  --set 'networkPolicy.extraIngress.api[0].ports[0].port=8080' \
  >"${EXTRA_INGRESS_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string app.credentialMasterKey=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
  --set-string app.providerSecrets.provider_zeta=second-provider-key \
  --set-string app.providerSecrets.provider_alpha=synthetic-provider-key \
  >"${CREDENTIAL_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string runtimeSecret.existingSecret=ironrag-runtime-external \
  --set-string runtimeSecret.restartNonce=rotation-2026-07-10 \
  >"${EXISTING_SECRET_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string runtimeSecret.existingSecret=ironrag-runtime-external \
  --set-string runtimeSecret.restartNonce=oidc-runtime-rotation-1 \
  --set-string app.frontendOrigin=https://ironrag.example.test \
  --set-string app.oidc.issuerUrl=https://issuer.example.test/realms/graph \
  --set-string app.oidc.clientId=ironrag-graph \
  --set-string app.oidc.redirectUrl=https://ironrag.example.test/v1/iam/oidc/callback \
  --set-string app.oidc.requiredGroup=ironrag-users \
  >"${OIDC_RENDER}"

# The chart-managed runtime Secret has no field for the OIDC client secret
# (it must come from an operator-managed Secret instead), so enabling OIDC
# without runtimeSecret.existingSecret must fail the render rather than
# silently ship a Secret nothing can complete.
set +e
OIDC_WITHOUT_EXISTING_SECRET_OUTPUT="$("${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string app.oidc.issuerUrl=https://issuer.example.test/realms/graph \
  --set-string app.oidc.clientId=ironrag-graph \
  --set-string app.oidc.redirectUrl=https://ironrag.example.test/v1/iam/oidc/callback \
  --set-string app.oidc.requiredGroup=ironrag-users 2>&1)"
OIDC_WITHOUT_EXISTING_SECRET_STATUS=$?
set -e
if [ "${OIDC_WITHOUT_EXISTING_SECRET_STATUS}" -eq 0 ]; then
  printf 'app.oidc without runtimeSecret.existingSecret must fail the render, but it rendered:\n%s\n' \
    "${OIDC_WITHOUT_EXISTING_SECRET_OUTPUT}" >&2
  exit 1
fi
case "${OIDC_WITHOUT_EXISTING_SECRET_OUTPUT}" in
  *"runtimeSecret.existingSecret"*) ;;
  *)
    printf 'app.oidc render without runtimeSecret.existingSecret failed for the wrong reason:\n%s\n' \
      "${OIDC_WITHOUT_EXISTING_SECRET_OUTPUT}" >&2
    exit 1
    ;;
esac
printf 'validated app.oidc without runtimeSecret.existingSecret is rejected\n'

"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/filesystem-single-node.yaml" \
  --set-string runtimeSecret.existingSecret=ironrag-runtime-external \
  --set-string runtimeSecret.restartNonce=runtime-rotation-1 \
  --set-string bundledDependencySecret.existingSecret=ironrag-bundled-external \
  --set-string bundledDependencySecret.restartNonce=postgres-rotation-1 \
  --set-string dependencies.postgres.persistence.existingClaim=ironrag-postgres-data \
  --set-string dependencies.postgres.shmSize=2Gi \
  --set dependencies.postgres.maxConnections=300 \
  --set-string dependencies.postgres.sharedBuffers=3GB \
  --set-string dependencies.postgres.workMem=12MB \
  --set-string dependencies.postgres.resources.requests.memory=3Gi \
  --set-string dependencies.postgres.resources.limits.memory=6144Mi \
  --set app.ingestion.maxParallelJobsGlobal=12 \
  --set app.ingestion.maxParallelJobsPerWorkspace=12 \
  --set app.ingestion.maxParallelJobsPerLibrary=12 \
  --set app.ingestion.doclingPageBatchSize=5 \
  --set app.ingestion.doclingPageStreamWindowPages=5 \
  --set app.ingestion.doclingMaxConcurrency=1 \
  --set app.ingestion.graphExtractParallelismPerDocument=2 \
  --set-string storage.filesystem.existingClaim=ironrag-content-data \
  --set-string api.image.digest=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --set-string worker.image.digest=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --set-string web.image.digest=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  --set-string dependencies.postgres.image.digest=sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  --set-string dependencies.redis.image.digest=sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd \
  --set-string startup.wait.image.digest=sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee \
  >"${MIGRATION_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/bundled-s3.yaml" \
  --set-string bundledDependencySecret.existingSecret=ironrag-bundled-external \
  --set-string bundledDependencySecret.restartNonce=r2 \
  --set-string dependencies.postgres.persistence.existingClaim=ironrag-postgres-data \
  --set-string storage.s3.persistence.existingClaim=ironrag-s4-data \
  --set-string storage.s3.image.digest=sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff \
  >"${S4_MIGRATION_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-json 'startup.jobAnnotations={"argocd.argoproj.io/hook":"Sync","argocd.argoproj.io/hook-delete-policy":"BeforeHookCreation"}' \
  --set-json startup.ttlSecondsAfterFinished=null \
  >"${ARGO_HOOK_RENDER}"

grep -q 'argocd.argoproj.io/hook: Sync' "${ARGO_HOOK_RENDER}"
grep -q 'argocd.argoproj.io/hook-delete-policy: BeforeHookCreation' "${ARGO_HOOK_RENDER}"
if grep -q 'ttlSecondsAfterFinished:' "${ARGO_HOOK_RENDER}"; then
  echo "Argo-retained startup Job must not have a TTL" >&2
  exit 1
fi

if grep -E -n '127\.0\.0\.11|ironrag-(backend|frontend):0\.3\.1|pipingspace/ironrag-(backend|frontend):0\.3\.1' \
  "${ROOT_DIR}/apps/web/nginx.conf.template" \
  "${BUNDLED_RENDER}" \
  "${FILESYSTEM_RENDER}" \
  "${EXTERNAL_RENDER}"
then
  echo "rendered Helm chart or web nginx template contains obsolete Docker-only DNS or image tags" >&2
  exit 1
fi

for rendered in \
  "${BUNDLED_RENDER}" \
  "${FILESYSTEM_RENDER}" \
  "${EXTERNAL_RENDER}"
do
  if grep -E -n '^kind: (NetworkPolicy|PodDisruptionBudget|HorizontalPodAutoscaler)$' "${rendered}"; then
    echo "optional hardening resources must be disabled by default in ${rendered}" >&2
    exit 1
  fi
done

python3 - \
  "${HARDENING_RENDER}" \
  "${EXTRA_INGRESS_RENDER}" \
  "${BUNDLED_RENDER}" \
  "${CREDENTIAL_RENDER}" \
  "${EXISTING_SECRET_RENDER}" \
  "${MIGRATION_RENDER}" \
  "${S4_MIGRATION_RENDER}" \
  "${OIDC_RENDER}" <<'PY'
import base64
import re
import sys

def load_documents(path):
    with open(path, encoding="utf-8") as rendered_file:
        return [
            document
            for document in re.split(
                r"^---\s*$", rendered_file.read(), flags=re.MULTILINE
            )
            if document.strip()
        ]


documents = load_documents(sys.argv[1])


def documents_of_kind(kind):
    marker = f"kind: {kind}"
    return [document for document in documents if re.search(rf"^{re.escape(marker)}$", document, re.MULTILINE)]


network_policies = documents_of_kind("NetworkPolicy")
assert len(network_policies) == 2, (
    f"expected two NetworkPolicy resources, got {len(network_policies)}"
)
for network_policy in network_policies:
    assert "podSelector:\n    matchLabels:\n      app.kubernetes.io/name: ironrag\n      app.kubernetes.io/instance: ironrag" in network_policy
    assert "- Ingress" in network_policy
    assert "- Egress" not in network_policy
web_network_policy = [policy for policy in network_policies if "name: ironrag-ironrag-web" in policy]
assert len(web_network_policy) == 1, "missing web NetworkPolicy"
assert "app.kubernetes.io/component: web" in web_network_policy[0]
assert "port: http" in web_network_policy[0]

for kind in ("PodDisruptionBudget", "HorizontalPodAutoscaler"):
    resources = documents_of_kind(kind)
    assert len(resources) == 3, f"expected three {kind} resources, got {len(resources)}"
    for component in ("api", "worker", "web"):
        expected_name = f"name: ironrag-ironrag-{component}"
        matches = [resource for resource in resources if expected_name in resource]
        assert len(matches) == 1, f"missing {kind} for {component}"
        resource = matches[0]
        assert f"app.kubernetes.io/component: {component}" in resource
        if kind == "PodDisruptionBudget":
            assert "maxUnavailable: 1" in resource
            selector = (
                "selector:\n"
                "    matchLabels:\n"
                "      app.kubernetes.io/name: ironrag\n"
                "      app.kubernetes.io/instance: ironrag\n"
                f"      app.kubernetes.io/component: {component}"
            )
            assert selector in resource
        else:
            scale_target = (
                "scaleTargetRef:\n"
                "    apiVersion: apps/v1\n"
                "    kind: Deployment\n"
                f"    name: ironrag-ironrag-{component}"
            )
            assert scale_target in resource
            assert "averageUtilization: 70" in resource

deployments = documents_of_kind("Deployment")
for component in ("api", "worker", "web"):
    expected_name = f"name: ironrag-ironrag-{component}"
    deployment = [resource for resource in deployments if expected_name in resource]
    assert len(deployment) == 1, f"missing Deployment for {component}"
    assert not re.search(r"^  replicas:", deployment[0], re.MULTILINE), (
        f"HPA-managed Deployment {component} must not pin spec.replicas"
    )

runtime_configs = [
    document
    for document in documents_of_kind("ConfigMap")
    if "name: ironrag-ironrag-runtime" in document
]
assert len(runtime_configs) == 1, "missing runtime ConfigMap"
assert 'IRONRAG_API_REPLICAS: "11"' in runtime_configs[0]
assert 'IRONRAG_WORKER_REPLICAS: "10"' in runtime_configs[0]
assert 'IRONRAG_QUERY_RERANK_ENABLED: "true"' in runtime_configs[0]
assert 'IRONRAG_QUERY_RERANK_CANDIDATE_LIMIT: "24"' in runtime_configs[0]
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_MODE: "off"' in runtime_configs[0]
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_TIMEOUT_MS: "1000"' in runtime_configs[0]
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_CANDIDATE_LIMIT: "16"' in runtime_configs[0]
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_CANDIDATE_TEXT_CHARS: "1200"' in runtime_configs[0]
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_TOTAL_TEXT_CHARS: "18000"' in runtime_configs[0]
assert not re.search(r"^\s*IRONRAG_OIDC_[A-Z_]+:", runtime_configs[0], re.MULTILINE), (
    "default runtime ConfigMap must omit disabled OIDC settings"
)

extra_ingress_documents = load_documents(sys.argv[2])
api_extra_policies = [
    document
    for document in extra_ingress_documents
    if "kind: NetworkPolicy" in document
    and "name: ironrag-ironrag-api-extra" in document
]
assert len(api_extra_policies) == 1, "missing component-scoped API ingress policy"
api_extra_policy = api_extra_policies[0]
assert "app.kubernetes.io/component: api" in api_extra_policy
assert "access: clients" in api_extra_policy
assert "port: 8080" in api_extra_policy
assert "app.kubernetes.io/component: worker" not in api_extra_policy

default_documents = load_documents(sys.argv[3])
for component in ("api", "worker", "web"):
    expected_name = f"name: ironrag-ironrag-{component}"
    deployment = [
        document
        for document in default_documents
        if "kind: Deployment" in document and expected_name in document
    ]
    assert len(deployment) == 1, f"missing default Deployment for {component}"
    assert re.search(r"^  replicas: 2$", deployment[0], re.MULTILINE), (
        f"non-HPA Deployment {component} must retain its configured replica count"
    )

default_postgres = [
    document
    for document in default_documents
    if "kind: Deployment" in document
    and "app.kubernetes.io/component: postgres" in document
]
assert len(default_postgres) == 1, "missing default PostgreSQL Deployment"
for postgres_arg in (
    '"max_connections=100"',
    '"shared_buffers=128MB"',
    '"work_mem=4MB"',
):
    assert postgres_arg in default_postgres[0], (
        f"default PostgreSQL workload is missing {postgres_arg}"
    )


def assert_runtime_rollout_annotations(
    rendered_documents, *, inline_secret, restart_nonce=None
):
    for kind, component in (
        ("Deployment", "api"),
        ("Deployment", "worker"),
        ("Job", "startup"),
    ):
        workloads = [
            document
            for document in rendered_documents
            if re.search(rf"^kind: {kind}$", document, re.MULTILINE)
            and f"app.kubernetes.io/component: {component}" in document
        ]
        assert len(workloads) == 1, f"missing {kind} workload for {component}"
        workload = workloads[0]
        assert "checksum/runtime-config:" in workload, (
            f"{component} must restart when the runtime ConfigMap changes"
        )
        if inline_secret:
            assert "checksum/runtime-secret:" in workload, (
                f"{component} must restart when the inline runtime Secret changes"
            )
            assert "runtime-secret-restart-nonce:" not in workload
        else:
            assert "checksum/runtime-secret:" not in workload
            assert re.search(
                rf"^\s*ironrag-runtime-secret-restart-nonce:\s+['\"]?{re.escape(restart_nonce)}['\"]?\s*$",
                workload,
                re.MULTILINE,
            ), f"{component} must expose the external Secret restart nonce"


assert_runtime_rollout_annotations(default_documents, inline_secret=True)

def workload_pod_spec(workload):
    marker = "    spec:\n"
    _, found, pod_spec = workload.partition(marker)
    assert found, "workload must contain a pod spec"
    return pod_spec


def named_container_sections(pod_spec, section_name):
    marker = f"      {section_name}:\n"
    _, found, remainder = pod_spec.partition(marker)
    if not found:
        return []
    next_section = re.search(r"^      [A-Za-z]", remainder, re.MULTILINE)
    section = remainder[: next_section.start()] if next_section else remainder
    return [
        block
        for block in re.split(r"^        - name: ", section, flags=re.MULTILINE)[1:]
        if block.strip()
    ]


kube_api_mount_path = "/var/run/secrets/kubernetes.io/serviceaccount"
for kind, component, expected_init_mounts in (
    ("Deployment", "api", 1),
    ("Deployment", "worker", 1),
    ("Job", "startup", 3),
):
    workloads = [
        document
        for document in default_documents
        if re.search(rf"^kind: {kind}$", document, re.MULTILINE)
        and f"app.kubernetes.io/component: {component}" in document
    ]
    assert len(workloads) == 1, f"missing {kind} workload for {component}"
    workload = workloads[0]
    assert "automountServiceAccountToken: false" in workload, (
        f"{component} must disable the implicit service-account token mount"
    )
    pod_spec = workload_pod_spec(workload)
    init_containers = named_container_sections(pod_spec, "initContainers")
    runtime_containers = named_container_sections(pod_spec, "containers")
    assert len(runtime_containers) == 1, f"{component} must have one runtime container"
    assert all(
        kube_api_mount_path not in container
        and "startup-wait-kube-api-access" not in container
        for container in runtime_containers
    ), f"{component} runtime container must not receive Kubernetes API credentials"
    credentialed_init_containers = [
        container
        for container in init_containers
        if f"mountPath: {kube_api_mount_path}" in container
        and "name: startup-wait-kube-api-access" in container
        and "readOnly: true" in container
    ]
    assert len(credentialed_init_containers) == expected_init_mounts, (
        f"{component} must mount read-only Kubernetes credentials only in kubectl init containers"
    )
    volumes_section = pod_spec.partition("      volumes:\n")[2]
    assert volumes_section.count("name: startup-wait-kube-api-access") == 1, (
        f"{component} must define exactly one projected Kubernetes API volume"
    )
    for required_projection in (
        "defaultMode: 0444",
        "serviceAccountToken:",
        "expirationSeconds: 3600",
        "path: token",
        "configMap:",
        "name: kube-root-ca.crt",
        "key: ca.crt",
        "path: ca.crt",
        "downwardAPI:",
        "path: namespace",
        "fieldPath: metadata.namespace",
    ):
        assert required_projection in volumes_section, (
            f"{component} projected Kubernetes API volume is missing {required_projection}"
        )

credential_documents = load_documents(sys.argv[4])
runtime_secrets = [
    document
    for document in credential_documents
    if "kind: Secret" in document and "name: ironrag-ironrag-runtime" in document
]
assert len(runtime_secrets) == 1, "missing runtime Secret for credential key"
assert (
    'IRONRAG_CREDENTIAL_MASTER_KEY: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="'
    in runtime_secrets[0]
), "credential master key was not rendered into runtime Secret"
assert 'IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64:' in runtime_secrets[0], (
    "inline provider key was not rendered into runtime Secret"
)
provider_map_match = re.search(
    r'^\s*IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64:\s+"([A-Za-z0-9+/=]+)"\s*$',
    runtime_secrets[0],
    re.MULTILINE,
)
assert provider_map_match, "provider JSON base64 value was not rendered consistently"
assert base64.b64decode(provider_map_match.group(1), validate=True).decode("utf-8") == (
    '{"provider_alpha":"synthetic-provider-key","provider_zeta":"second-provider-key"}'
), "provider map JSON must render in deterministic key order"
credential_runtime_configs = [
    document
    for document in credential_documents
    if "kind: ConfigMap" in document and "name: ironrag-ironrag-runtime" in document
]
assert len(credential_runtime_configs) == 1, "missing credential runtime ConfigMap"
assert all(
    "IRONRAG_CREDENTIAL_MASTER_KEY" not in document
    for document in credential_documents
    if "kind: ConfigMap" in document
), "credential master key must never be rendered into a ConfigMap"

existing_secret_documents = load_documents(sys.argv[5])
assert not any(
    "kind: Secret" in document
    and "name: ironrag-ironrag-runtime" in document
    for document in existing_secret_documents
), "an external runtime Secret must suppress the inline Secret"
assert_runtime_rollout_annotations(
    existing_secret_documents,
    inline_secret=False,
    restart_nonce="rotation-2026-07-10",
)

oidc_documents = load_documents(sys.argv[8])
oidc_runtime_configs = [
    document
    for document in oidc_documents
    if "kind: ConfigMap" in document
    and "name: ironrag-ironrag-runtime" in document
]
assert len(oidc_runtime_configs) == 1, "missing OIDC runtime ConfigMap"
oidc_runtime_config = oidc_runtime_configs[0]
expected_oidc_settings = {
    "IRONRAG_OIDC_ISSUER_URL": "https://issuer.example.test/realms/graph",
    "IRONRAG_OIDC_CLIENT_ID": "ironrag-graph",
    "IRONRAG_OIDC_REDIRECT_URL": "https://ironrag.example.test/v1/iam/oidc/callback",
    "IRONRAG_OIDC_REQUIRED_GROUP": "ironrag-users",
}
rendered_oidc_settings = set(
    re.findall(r"^\s*(IRONRAG_OIDC_[A-Z_]+):", oidc_runtime_config, re.MULTILINE)
)
assert rendered_oidc_settings == set(expected_oidc_settings), (
    "OIDC runtime ConfigMap must render exactly the four non-secret settings"
)
for oidc_env, oidc_value in expected_oidc_settings.items():
    assert f'{oidc_env}: "{oidc_value}"' in oidc_runtime_config, (
        f"OIDC runtime ConfigMap is missing {oidc_env}"
    )

migration_documents = load_documents(sys.argv[6])
assert not any("kind: PersistentVolumeClaim" in document for document in migration_documents), (
    "pre-created migration claims must suppress chart-owned PVCs"
)
assert not any("kind: Secret" in document for document in migration_documents), (
    "operator-managed runtime and bundled dependency Secrets must suppress inline Secrets"
)
migration_render = "\n".join(migration_documents)
assert "claimName: ironrag-postgres-data" in migration_render
assert "claimName: ironrag-content-data" in migration_render
assert "pipingspace/ironrag-backend@sha256:" + "a" * 64 in migration_render
assert "pipingspace/ironrag-frontend@sha256:" + "b" * 64 in migration_render
assert "pgvector/pgvector@sha256:" + "c" * 64 in migration_render
assert "redis@sha256:" + "d" * 64 in migration_render
assert "registry.k8s.io/kubectl@sha256:" + "e" * 64 in migration_render
migration_runtime_configs = [
    document
    for document in migration_documents
    if "kind: ConfigMap" in document
    and "name: ironrag-ironrag-runtime" in document
]
assert len(migration_runtime_configs) == 1
for runtime_setting in (
    'IRONRAG_INGESTION_MAX_PARALLEL_JOBS_GLOBAL: "12"',
    'IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_WORKSPACE: "12"',
    'IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_LIBRARY: "12"',
    'IRONRAG_DOCLING_PAGE_BATCH_SIZE: "5"',
    'IRONRAG_DOCLING_PAGE_STREAM_WINDOW_PAGES: "5"',
    'IRONRAG_DOCLING_MAX_CONCURRENCY: "1"',
    'IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM: "auto"',
    'IRONRAG_INGESTION_EMBEDDING_PARALLELISM: "8"',
    'IRONRAG_INGESTION_GRAPH_EXTRACT_PARALLELISM_PER_DOC: "2"',
):
    assert runtime_setting in migration_runtime_configs[0], (
        f"migration runtime ConfigMap is missing {runtime_setting}"
    )
postgres_workloads = [
    document
    for document in migration_documents
    if "kind: Deployment" in document
    and "app.kubernetes.io/component: postgres" in document
]
assert len(postgres_workloads) == 1
postgres_workload = postgres_workloads[0]
assert 'ironrag-bundled-secret-restart-nonce: "postgres-rotation-1"' in postgres_workload
for postgres_arg in (
    '"max_connections=300"',
    '"shared_buffers=3GB"',
    '"work_mem=12MB"',
):
    assert postgres_arg in postgres_workload, (
        f"migration PostgreSQL workload is missing {postgres_arg}"
    )
assert "sizeLimit: 2Gi" in postgres_workload
assert re.search(
    r"resources:\n\s+limits:\n(?:\s+[^\n]+\n)*?\s+memory: 6144Mi",
    postgres_workload,
), "migration PostgreSQL workload must preserve its 6 GiB memory limit"
assert re.search(
    r"resources:\n(?:\s+[^\n]+\n)*?\s+requests:\n(?:\s+[^\n]+\n)*?\s+memory: 3Gi",
    postgres_workload,
), "migration PostgreSQL workload must reserve its shared-buffer budget"

s4_migration_documents = load_documents(sys.argv[7])
assert not any("kind: PersistentVolumeClaim" in document for document in s4_migration_documents)
s4_migration_render = "\n".join(s4_migration_documents)
assert "claimName: ironrag-postgres-data" in s4_migration_render
assert "claimName: ironrag-s4-data" in s4_migration_render
assert "s4core/s4core@sha256:" + "f" * 64 in s4_migration_render
s4_workloads = [
    document
    for document in s4_migration_documents
    if "kind: Deployment" in document
    and "app.kubernetes.io/component: s4core" in document
]
assert len(s4_workloads) == 1
assert 'ironrag-bundled-secret-restart-nonce: "r2"' in s4_workloads[0]
PY

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string app.credentialMasterKey=invalid >/dev/null 2>&1
then
  echo "Helm validation accepted an invalid credential master key" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string app.providerSecrets.provider_alpha=synthetic-provider-key >/dev/null 2>&1
then
  echo "Helm validation accepted an inline provider secret without a credential master key" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string runtimeSecret.existingSecret=ironrag-runtime-external \
  --set runtimeSecret.restartNonce=123 >/dev/null 2>&1
then
  echo "Helm schema accepted a non-string runtime Secret restart nonce" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string runtimeSecret.existingSecret=ironrag-runtime-external \
  --set-string runtimeSecret.restartNonce=inline-conflict-check \
  --set-string app.credentialMasterKey=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
  >/dev/null 2>&1
then
  echo "Helm validation accepted an inline credential key that an existing runtime Secret would ignore" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string runtimeSecret.existingSecret=ironrag-runtime-external >/dev/null 2>&1
then
  echo "Helm schema accepted an external runtime Secret without an explicit restart nonce" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string app.oidc.issuerUrl=https://issuer.example.test/realms/graph \
  >/dev/null 2>&1
then
  echo "Helm validation accepted a partial OIDC configuration" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string bundledDependencySecret.existingSecret=ironrag-bundled-external \
  >/dev/null 2>&1
then
  echo "Helm schema accepted an external bundled dependency Secret without an explicit restart nonce" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string api.image.digest=sha256:not-a-digest >/dev/null 2>&1
then
  echo "Helm validation accepted an invalid application image digest" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string dependencies.postgres.maxConnections=not-an-integer >/dev/null 2>&1
then
  echo "Helm schema accepted a non-integer PostgreSQL connection limit" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string dependencies.postgres.sharedBuffers=3Gi >/dev/null 2>&1
then
  echo "Helm schema accepted an unsupported PostgreSQL memory unit" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string dependencies.postgres.shmSize=4096MB >/dev/null 2>&1
then
  echo "Helm schema accepted an unsupported Kubernetes shmSize unit" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-json dependencies.postgres.workMem=null >/dev/null 2>&1
then
  echo "Helm schema accepted a missing PostgreSQL memory setting" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set app.ingestion.doclingMaxConcurrency=0 >/dev/null 2>&1
then
  echo "Helm schema accepted zero Docling concurrency" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set app.ingestion.maxParallelJobsGlobal=4 \
  --set app.ingestion.maxParallelJobsPerWorkspace=5 >/dev/null 2>&1
then
  echo "Helm validation accepted workspace ingestion concurrency above global" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set app.ingestion.maxParallelJobsPerWorkspace=2 \
  --set app.ingestion.maxParallelJobsPerLibrary=3 >/dev/null 2>&1
then
  echo "Helm validation accepted library ingestion concurrency above workspace" >&2
  exit 1
fi

"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string runtimeSecret.existingSecret=ironrag-runtime-external \
  --set-string runtimeSecret.restartNonce=initial-rollout >/dev/null

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set observability.enabled=true >/dev/null 2>&1
then
  echo "Helm validation accepted enabled OTLP export without an endpoint" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string observability.logsExporter=typo >/dev/null 2>&1
then
  echo "Helm schema accepted an unsupported OTEL logs exporter" >&2
  exit 1
fi

"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set observability.enabled=true \
  --set-string observability.otlpEndpoint=http://collector.example:4317 \
  --set-string observability.otlpProtocol=grpc >/dev/null

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string app.queryRerank.semantic.mode=typo >/dev/null 2>&1
then
  echo "Helm schema accepted an unsupported semantic rerank mode" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set app.queryRerank.enabled=false \
  --set-string app.queryRerank.semantic.mode=active >/dev/null 2>&1
then
  echo "Helm validation accepted active semantic reranking with the master rerank gate disabled" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set app.queryRerank.semantic.timeoutMs=5001 >/dev/null 2>&1
then
  echo "Helm schema accepted a semantic rerank timeout above the runtime hard cap" >&2
  exit 1
fi

"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string app.queryRerank.semantic.mode=active >/dev/null

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set autoscaling.enabled=true \
  --set app.databaseMaxConnections=84 \
  --set autoscaling.api.minReplicas=5 \
  --set autoscaling.api.maxReplicas=2 >/dev/null 2>&1
then
  echo "Helm validation accepted autoscaling.api.maxReplicas < minReplicas" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set autoscaling.enabled=true >/dev/null 2>&1
then
  echo "Helm validation accepted an HPA capacity larger than the database connection budget" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/filesystem-single-node.yaml" \
  --set autoscaling.enabled=true \
  --set app.databaseMaxConnections=84 >/dev/null 2>&1
then
  echo "Helm validation accepted API/worker autoscaling with single-node filesystem storage" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set podDisruptionBudget.enabled=true \
  --set podDisruptionBudget.api.minAvailable=1 >/dev/null 2>&1
then
  echo "Helm validation accepted both PDB minAvailable and maxUnavailable" >&2
  exit 1
fi

"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set podDisruptionBudget.enabled=true \
  --set podDisruptionBudget.api.maxUnavailable=0 >/dev/null

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set podDisruptionBudget.enabled=true \
  --set-string podDisruptionBudget.api.maxUnavailable= \
  --set podDisruptionBudget.api.minAvailable=true >/dev/null 2>&1
then
  echo "Helm schema accepted a boolean PDB minAvailable" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set podDisruptionBudget.enabled=true \
  --set-string podDisruptionBudget.api.maxUnavailable= \
  --set podDisruptionBudget.api.minAvailable=-1 >/dev/null 2>&1
then
  echo "Helm schema accepted a negative PDB minAvailable" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set autoscaling.enabled=true \
  --set app.databaseMaxConnections=84 \
  --set autoscaling.api.minReplicas=true >/dev/null 2>&1
then
  echo "Helm schema accepted a boolean HPA minReplicas" >&2
  exit 1
fi

"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set autoscaling.enabled=true \
  --set app.databaseMaxConnections=84 \
  --set autoscaling.api.targetCPUUtilizationPercentage=150 >/dev/null

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set networkPolicy.enabled=true \
  --set 'networkPolicy.extraIngress.api[0].form[0].namespaceSelector.matchLabels.access=clients' \
  --set 'networkPolicy.extraIngress.api[0].ports[0].port=8080' >/dev/null 2>&1
then
  echo "Helm schema accepted an unknown NetworkPolicy ingress key" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set networkPolicy.enabled=true \
  --set 'networkPolicy.extraIngress.api[0].from[0].namespaceSelector.matchLabels.access=clients' \
  >/dev/null 2>&1
then
  echo "Helm schema accepted a component ingress rule without explicit ports" >&2
  exit 1
fi

if ! grep -F -q "pipingspace/ironrag-backend:${APP_IMAGE_TAG}" "${BUNDLED_RENDER}"; then
  echo "rendered Helm chart does not contain the backend image tag derived from Chart.appVersion" >&2
  exit 1
fi

if ! grep -F -q "pipingspace/ironrag-frontend:${APP_IMAGE_TAG}" "${BUNDLED_RENDER}"; then
  echo "rendered Helm chart does not contain the frontend image tag derived from Chart.appVersion" >&2
  exit 1
fi

for rendered in "${BUNDLED_RENDER}" "${FILESYSTEM_RENDER}"; do
  if ! grep -F -q 'image: "pgvector/pgvector:pg18"' "${rendered}"; then
    echo "rendered Helm chart ${rendered} is missing bundled postgres image pgvector/pgvector:pg18" >&2
    exit 1
  fi
  if ! grep -F -q 'image: "redis:8.10.1"' "${rendered}"; then
    echo "rendered Helm chart ${rendered} is missing bundled redis image redis:8.10.1" >&2
    exit 1
  fi
done

printf 'validated %s\n' "${BUNDLED_RENDER}"
printf 'validated %s\n' "${FILESYSTEM_RENDER}"
printf 'validated %s\n' "${EXTERNAL_RENDER}"
printf 'validated %s\n' "${HARDENING_RENDER}"
printf 'validated %s\n' "${EXTRA_INGRESS_RENDER}"
