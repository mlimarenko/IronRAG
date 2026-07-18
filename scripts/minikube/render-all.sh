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
  --set app.credentialEncryptionWriteEnabled=true \
  --set-string app.providerSecrets.provider_zeta=second-provider-key \
  --set-string app.providerSecrets.provider_alpha=synthetic-provider-key \
  >"${CREDENTIAL_RENDER}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set-string runtimeSecret.existingSecret=ironrag-runtime-external \
  --set-string runtimeSecret.restartNonce=rotation-2026-07-10 \
  >"${EXISTING_SECRET_RENDER}"

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
  "${EXISTING_SECRET_RENDER}" <<'PY'
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
assert "port: 80" in web_network_policy[0]

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
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_TIMEOUT_MS: "1500"' in runtime_configs[0]
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_CANDIDATE_LIMIT: "16"' in runtime_configs[0]
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_CANDIDATE_TEXT_CHARS: "1200"' in runtime_configs[0]
assert 'IRONRAG_QUERY_SEMANTIC_RERANK_TOTAL_TEXT_CHARS: "18000"' in runtime_configs[0]
assert 'IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED: "false"' in runtime_configs[0]

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
assert provider_map_match, "provider JSON base64 value was not rendered canonically"
assert base64.b64decode(provider_map_match.group(1), validate=True).decode("utf-8") == (
    '{"provider_alpha":"synthetic-provider-key","provider_zeta":"second-provider-key"}'
), "provider map JSON must render in deterministic key order"
credential_runtime_configs = [
    document
    for document in credential_documents
    if "kind: ConfigMap" in document and "name: ironrag-ironrag-runtime" in document
]
assert len(credential_runtime_configs) == 1, "missing credential runtime ConfigMap"
assert (
    'IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED: "true"'
    in credential_runtime_configs[0]
), "credential write gate was not rendered into the runtime ConfigMap"
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
  --set-string app.credentialEncryptionWriteEnabled=true >/dev/null 2>&1
then
  echo "Helm schema accepted a string credential write gate" >&2
  exit 1
fi

if "${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --set app.credentialEncryptionWriteEnabled=true >/dev/null 2>&1
then
  echo "Helm validation accepted credential writes without an inline master key" >&2
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
  --set app.queryRerank.semantic.timeoutMs=3001 >/dev/null 2>&1
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
  if ! grep -F -q 'image: "redis:8.8"' "${rendered}"; then
    echo "rendered Helm chart ${rendered} is missing bundled redis image redis:8.8" >&2
    exit 1
  fi
done

printf 'validated %s\n' "${BUNDLED_RENDER}"
printf 'validated %s\n' "${FILESYSTEM_RENDER}"
printf 'validated %s\n' "${EXTERNAL_RENDER}"
printf 'validated %s\n' "${HARDENING_RENDER}"
printf 'validated %s\n' "${EXTRA_INGRESS_RENDER}"
