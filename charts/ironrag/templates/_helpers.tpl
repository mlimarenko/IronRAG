{{- define "ironrag.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "ironrag.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name (include "ironrag.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "ironrag.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" -}}
{{- end -}}

{{- define "ironrag.labels" -}}
helm.sh/chart: {{ include "ironrag.chart" . }}
app.kubernetes.io/name: {{ include "ironrag.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{- define "ironrag.selectorLabels" -}}
app.kubernetes.io/name: {{ include "ironrag.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "ironrag.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "ironrag.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "ironrag.postgresHost" -}}
{{- if eq .Values.dependencies.postgres.mode "bundled" -}}
{{ include "ironrag.fullname" . }}-postgres
{{- else -}}
{{ required "dependencies.postgres.host is required when dependencies.postgres.mode=external and url is not provided" .Values.dependencies.postgres.host }}
{{- end -}}
{{- end -}}

{{- define "ironrag.redisHost" -}}
{{- if eq .Values.dependencies.redis.mode "bundled" -}}
{{ include "ironrag.fullname" . }}-redis
{{- else -}}
{{ required "dependencies.redis.host is required when dependencies.redis.mode=external and url is not provided" .Values.dependencies.redis.host }}
{{- end -}}
{{- end -}}

{{- define "ironrag.databaseUrl" -}}
{{- if .Values.dependencies.postgres.url -}}
{{ .Values.dependencies.postgres.url }}
{{- else -}}
{{- printf "postgres://%s:%s@%s:%v/%s" .Values.dependencies.postgres.username .Values.dependencies.postgres.password (include "ironrag.postgresHost" .) .Values.dependencies.postgres.port .Values.dependencies.postgres.database -}}
{{- end -}}
{{- end -}}

{{- define "ironrag.redisUrl" -}}
{{- if .Values.dependencies.redis.url -}}
{{ .Values.dependencies.redis.url }}
{{- else -}}
{{- printf "redis://%s:%v" (include "ironrag.redisHost" .) .Values.dependencies.redis.port -}}
{{- end -}}
{{- end -}}

{{- define "ironrag.objectStorageMode" -}}
{{- if eq .Values.storage.provider "filesystem" -}}
disabled
{{- else -}}
{{- .Values.storage.s3.mode -}}
{{- end -}}
{{- end -}}

{{- define "ironrag.s3Endpoint" -}}
{{- if eq .Values.storage.provider "filesystem" -}}

{{- else if eq .Values.storage.s3.mode "bundled" -}}
{{- printf "http://%s-s4core:9000" (include "ironrag.fullname" .) -}}
{{- else -}}
{{- required "storage.s3.endpoint is required when storage.provider=s3 and storage.s3.mode=external" .Values.storage.s3.endpoint -}}
{{- end -}}
{{- end -}}

{{- define "ironrag.apiUpstream" -}}
{{- printf "http://%s-api:%v" (include "ironrag.fullname" .) .Values.api.service.port -}}
{{- end -}}

{{- define "ironrag.appImage" -}}
{{- $root := .root -}}
{{- $image := .image -}}
{{- $repository := required "image.repository is required" $image.repository -}}
{{- printf "%s:%s" $repository (default (printf "v%s" $root.Chart.AppVersion) $image.tag) -}}
{{- end -}}

{{- define "ironrag.runtimeSecretName" -}}
{{- if .Values.runtimeSecret.existingSecret -}}
{{ .Values.runtimeSecret.existingSecret }}
{{- else -}}
{{ include "ironrag.fullname" . }}-runtime
{{- end -}}
{{- end -}}

{{- define "ironrag.bundledDependencySecretName" -}}
{{ include "ironrag.fullname" . }}-bundled-dependencies
{{- end -}}

{{- define "ironrag.runtimePodAnnotations" -}}
{{- $annotations := deepCopy (default (dict) .Values.podAnnotations) -}}
{{- $_ := unset $annotations "checksum/runtime-config" -}}
{{- $_ := unset $annotations "checksum/runtime-secret" -}}
{{- $_ := unset $annotations "ironrag-runtime-secret-restart-nonce" -}}
{{- $_ := set $annotations "checksum/runtime-config" (include (print $.Template.BasePath "/runtime-config.yaml") . | sha256sum) -}}
{{- if .Values.runtimeSecret.existingSecret -}}
{{- $_ := set $annotations "ironrag-runtime-secret-restart-nonce" .Values.runtimeSecret.restartNonce -}}
{{- else -}}
{{- $_ := set $annotations "checksum/runtime-secret" (include (print $.Template.BasePath "/runtime-secret.yaml") . | sha256sum) -}}
{{- end -}}
{{- toYaml $annotations -}}
{{- end -}}

{{- define "ironrag.startupJobName" -}}
{{- printf "%s-startup-r%d" (include "ironrag.fullname" .) .Release.Revision | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "ironrag.apiCapacityReplicas" -}}
{{- if and .Values.autoscaling.enabled .Values.autoscaling.api.enabled -}}
{{- add (int .Values.autoscaling.api.maxReplicas) 1 -}}
{{- else -}}
{{- add (int .Values.api.replicaCount) 1 -}}
{{- end -}}
{{- end -}}

{{- define "ironrag.workerCapacityReplicas" -}}
{{- if and .Values.autoscaling.enabled .Values.autoscaling.worker.enabled -}}
{{- int .Values.autoscaling.worker.maxReplicas -}}
{{- else -}}
{{- int .Values.worker.replicaCount -}}
{{- end -}}
{{- end -}}

{{- define "ironrag.startupWaitKubeApiVolumeMount" -}}
- name: startup-wait-kube-api-access
  mountPath: /var/run/secrets/kubernetes.io/serviceaccount
  readOnly: true
{{- end -}}

{{- define "ironrag.startupWaitKubeApiVolume" -}}
- name: startup-wait-kube-api-access
  projected:
    defaultMode: 0444
    sources:
      - serviceAccountToken:
          expirationSeconds: 3600
          path: token
      - configMap:
          name: kube-root-ca.crt
          items:
            - key: ca.crt
              path: ca.crt
      - downwardAPI:
          items:
            - path: namespace
              fieldRef:
                apiVersion: v1
                fieldPath: metadata.namespace
{{- end -}}

{{- define "ironrag.startupDependencyWaitInitContainer" -}}
{{- if eq .Values.dependencies.postgres.mode "bundled" }}
- name: wait-for-bundled-postgres
  image: "{{ .Values.startup.wait.image.repository }}:{{ .Values.startup.wait.image.tag }}"
  imagePullPolicy: {{ .Values.startup.wait.image.pullPolicy }}
  volumeMounts:
    {{- include "ironrag.startupWaitKubeApiVolumeMount" . | nindent 4 }}
  securityContext:
    runAsNonRoot: true
    runAsUser: 65532
    runAsGroup: 65532
    readOnlyRootFilesystem: true
    allowPrivilegeEscalation: false
    capabilities:
      drop:
        - ALL
  command:
    - kubectl
  args:
    - wait
    - --namespace={{ .Release.Namespace }}
    - --for=condition=available
    - --timeout={{ .Values.startup.wait.timeoutSeconds }}s
    - deployment/{{ include "ironrag.fullname" . }}-postgres
{{- end }}
{{- if eq .Values.dependencies.redis.mode "bundled" }}
- name: wait-for-bundled-redis
  image: "{{ .Values.startup.wait.image.repository }}:{{ .Values.startup.wait.image.tag }}"
  imagePullPolicy: {{ .Values.startup.wait.image.pullPolicy }}
  volumeMounts:
    {{- include "ironrag.startupWaitKubeApiVolumeMount" . | nindent 4 }}
  securityContext:
    runAsNonRoot: true
    runAsUser: 65532
    runAsGroup: 65532
    readOnlyRootFilesystem: true
    allowPrivilegeEscalation: false
    capabilities:
      drop:
        - ALL
  command:
    - kubectl
  args:
    - wait
    - --namespace={{ .Release.Namespace }}
    - --for=condition=available
    - --timeout={{ .Values.startup.wait.timeoutSeconds }}s
    - deployment/{{ include "ironrag.fullname" . }}-redis
{{- end }}
{{- if and (eq .Values.storage.provider "s3") (eq .Values.storage.s3.mode "bundled") }}
- name: wait-for-bundled-s4core
  image: "{{ .Values.startup.wait.image.repository }}:{{ .Values.startup.wait.image.tag }}"
  imagePullPolicy: {{ .Values.startup.wait.image.pullPolicy }}
  volumeMounts:
    {{- include "ironrag.startupWaitKubeApiVolumeMount" . | nindent 4 }}
  securityContext:
    runAsNonRoot: true
    runAsUser: 65532
    runAsGroup: 65532
    readOnlyRootFilesystem: true
    allowPrivilegeEscalation: false
    capabilities:
      drop:
        - ALL
  command:
    - kubectl
  args:
    - wait
    - --namespace={{ .Release.Namespace }}
    - --for=condition=available
    - --timeout={{ .Values.startup.wait.timeoutSeconds }}s
    - deployment/{{ include "ironrag.fullname" . }}-s4core
{{- end }}
{{- end -}}

{{- define "ironrag.startupWaitInitContainer" -}}
{{- if eq .Values.startup.mode "startup_job" }}
- name: wait-for-startup
  image: "{{ .Values.startup.wait.image.repository }}:{{ .Values.startup.wait.image.tag }}"
  imagePullPolicy: {{ .Values.startup.wait.image.pullPolicy }}
  volumeMounts:
    {{- include "ironrag.startupWaitKubeApiVolumeMount" . | nindent 4 }}
  securityContext:
    runAsNonRoot: true
    runAsUser: 65532
    runAsGroup: 65532
    readOnlyRootFilesystem: true
    allowPrivilegeEscalation: false
    capabilities:
      drop:
        - ALL
  command:
    - kubectl
  args:
    - wait
    - --namespace={{ .Release.Namespace }}
    - --for=condition=complete
    - --timeout={{ .Values.startup.wait.timeoutSeconds }}s
    - job/{{ include "ironrag.startupJobName" . }}
{{- end -}}
{{- end -}}

{{- define "ironrag.validate" -}}
{{- if and (eq .Values.storage.provider "filesystem") (ne .Values.storage.topology "single_node") -}}
{{- fail "storage.topology must be single_node when storage.provider=filesystem" -}}
{{- end -}}
{{- if and (eq .Values.storage.provider "filesystem") (or (gt (int .Values.api.replicaCount) 1) (gt (int .Values.worker.replicaCount) 1)) -}}
{{- fail "filesystem storage is only supported with api.replicaCount=1 and worker.replicaCount=1" -}}
{{- end -}}
{{- if and (eq .Values.storage.provider "filesystem") .Values.autoscaling.enabled (or (and .Values.autoscaling.api.enabled (gt (int .Values.autoscaling.api.maxReplicas) 1)) (and .Values.autoscaling.worker.enabled (gt (int .Values.autoscaling.worker.maxReplicas) 1))) -}}
{{- fail "filesystem storage does not support API or worker autoscaling above one replica" -}}
{{- end -}}
{{- if and (eq .Values.storage.provider "s3") (ne .Values.storage.topology "shared_cluster") -}}
{{- fail "storage.topology must be shared_cluster when storage.provider=s3" -}}
{{- end -}}
{{- if and (eq .Values.storage.provider "s3") (not (or (eq .Values.storage.s3.mode "bundled") (eq .Values.storage.s3.mode "external"))) -}}
{{- fail "storage.s3.mode must be bundled or external when storage.provider=s3" -}}
{{- end -}}
{{- if and (eq .Values.dependencies.postgres.mode "external") (empty .Values.runtimeSecret.existingSecret) (and (empty .Values.dependencies.postgres.url) (empty .Values.dependencies.postgres.host)) -}}
{{- fail "dependencies.postgres.url or dependencies.postgres.host is required when dependencies.postgres.mode=external and runtimeSecret.existingSecret is empty" -}}
{{- end -}}
{{- if and (eq .Values.dependencies.redis.mode "external") (empty .Values.runtimeSecret.existingSecret) (and (empty .Values.dependencies.redis.url) (empty .Values.dependencies.redis.host)) -}}
{{- fail "dependencies.redis.url or dependencies.redis.host is required when dependencies.redis.mode=external and runtimeSecret.existingSecret is empty" -}}
{{- end -}}
{{- if and (eq .Values.storage.provider "s3") (eq .Values.storage.s3.mode "external") (empty .Values.storage.s3.bucket) -}}
{{- fail "storage.s3.bucket is required when storage.provider=s3" -}}
{{- end -}}
{{- $providerSecrets := .Values.app.providerSecrets -}}
{{- $hasInlineProviderSecret := false -}}
{{- if gt (len $providerSecrets) 256 -}}
{{- fail "app.providerSecrets must contain at most 256 entries" -}}
{{- end -}}
{{- if gt (len (toJson $providerSecrets)) 1048576 -}}
{{- fail "app.providerSecrets JSON must not exceed 1048576 UTF-8 bytes" -}}
{{- end -}}
{{- range $providerKind, $apiKey := $providerSecrets -}}
{{- if or (eq (len $providerKind) 0) (gt (len $providerKind) 128) (regexMatch "[[:space:][:cntrl:]]" $providerKind) -}}
{{- fail (printf "app.providerSecrets key %q must be an exact provider kind of 1-128 UTF-8 bytes without whitespace or control characters" $providerKind) -}}
{{- end -}}
{{- if gt (len $apiKey) 65536 -}}
{{- fail "each app.providerSecrets credential must not exceed 65536 UTF-8 bytes" -}}
{{- end -}}
{{- if not (empty $apiKey) -}}
{{- $hasInlineProviderSecret = true -}}
{{- end -}}
{{- end -}}
{{- $hasInlineCredentialKeyring := or
  (not (empty .Values.app.credentialMasterKey))
  (not (empty .Values.app.credentialMasterKeyId))
  (not (empty .Values.app.credentialPreviousMasterKeys))
-}}
{{- if and (empty .Values.app.credentialMasterKey) (or (not (empty .Values.app.credentialMasterKeyId)) (not (empty .Values.app.credentialPreviousMasterKeys))) -}}
{{- fail "app.credentialMasterKey is required when an inline credential key ID or previous-key map is configured" -}}
{{- end -}}
{{- if and .Values.app.credentialEncryptionWriteEnabled (empty .Values.runtimeSecret.existingSecret) (empty .Values.app.credentialMasterKey) -}}
{{- fail "app.credentialMasterKey is required when app.credentialEncryptionWriteEnabled=true and runtimeSecret.existingSecret is empty" -}}
{{- end -}}
{{- if and (empty .Values.runtimeSecret.existingSecret) $hasInlineProviderSecret (empty .Values.app.credentialMasterKey) -}}
{{- fail "app.credentialMasterKey is required when app.providerSecrets contains an inline provider credential" -}}
{{- end -}}
{{- if and (not (empty .Values.runtimeSecret.existingSecret)) (or $hasInlineProviderSecret $hasInlineCredentialKeyring) -}}
{{- fail "app credential keyring and app.providerSecrets must stay empty when runtimeSecret.existingSecret is used because inline secret values would be ignored" -}}
{{- end -}}
{{- if and .Values.observability.enabled (empty .Values.observability.otlpEndpoint) -}}
{{- fail "observability.otlpEndpoint is required when observability.enabled=true" -}}
{{- end -}}
{{- if and (not .Values.app.queryRerank.enabled) (ne .Values.app.queryRerank.semantic.mode "off") -}}
{{- fail "app.queryRerank.enabled must be true when app.queryRerank.semantic.mode is shadow or active" -}}
{{- end -}}
{{- if .Values.podDisruptionBudget.enabled -}}
{{- range $component := list "api" "worker" "web" -}}
{{- $settings := index $.Values.podDisruptionBudget $component -}}
{{- if $settings.enabled -}}
{{- $hasMin := ne (toString $settings.minAvailable) "" -}}
{{- $hasMax := ne (toString $settings.maxUnavailable) "" -}}
{{- if eq $hasMin $hasMax -}}
{{- fail (printf "podDisruptionBudget.%s must set exactly one of minAvailable or maxUnavailable" $component) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if .Values.autoscaling.enabled -}}
{{- range $component := list "api" "worker" "web" -}}
{{- $settings := index $.Values.autoscaling $component -}}
{{- if $settings.enabled -}}
{{- if lt (int $settings.minReplicas) 1 -}}
{{- fail (printf "autoscaling.%s.minReplicas must be at least 1" $component) -}}
{{- end -}}
{{- if lt (int $settings.maxReplicas) (int $settings.minReplicas) -}}
{{- fail (printf "autoscaling.%s.maxReplicas must be greater than or equal to minReplicas" $component) -}}
{{- end -}}
{{- if and (empty $settings.targetCPUUtilizationPercentage) (empty $settings.targetMemoryUtilizationPercentage) -}}
{{- fail (printf "autoscaling.%s requires at least one CPU or memory utilization target" $component) -}}
{{- end -}}
{{- range $target := list $settings.targetCPUUtilizationPercentage $settings.targetMemoryUtilizationPercentage -}}
{{- if and (not (empty $target)) (lt (int $target) 1) -}}
{{- fail (printf "autoscaling.%s utilization targets must be greater than zero" $component) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{/* Keep this in sync with MIN_DATABASE_CONNECTIONS_PER_RUNTIME_REPLICA in apps/api/src/app/config.rs. */}}
{{- $runtimeReplicas := add (int (include "ironrag.apiCapacityReplicas" .)) (int (include "ironrag.workerCapacityReplicas" .)) -}}
{{- $minimumConnectionBudget := mul 4 $runtimeReplicas -}}
{{- if lt (int .Values.app.databaseMaxConnections) (int $minimumConnectionBudget) -}}
{{- fail (printf "app.databaseMaxConnections must be at least %d for the configured API/worker autoscaling maxima" (int $minimumConnectionBudget)) -}}
{{- end -}}
{{- end -}}
{{- end -}}
