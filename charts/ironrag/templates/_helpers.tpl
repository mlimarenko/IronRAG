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

{{- define "ironrag.arangodbHost" -}}
{{- if eq .Values.dependencies.arangodb.mode "bundled" -}}
{{ include "ironrag.fullname" . }}-arangodb
{{- else -}}
{{ required "dependencies.arangodb.host is required when dependencies.arangodb.mode=external and url is not provided" .Values.dependencies.arangodb.host }}
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

{{- define "ironrag.arangodbUrl" -}}
{{- if .Values.dependencies.arangodb.url -}}
{{ .Values.dependencies.arangodb.url }}
{{- else -}}
{{- printf "http://%s:%v" (include "ironrag.arangodbHost" .) .Values.dependencies.arangodb.port -}}
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

{{- define "ironrag.runtimeSecretName" -}}
{{- if .Values.runtimeSecret.existingSecret -}}
{{ .Values.runtimeSecret.existingSecret }}
{{- else -}}
{{ include "ironrag.fullname" . }}-runtime
{{- end -}}
{{- end -}}

{{- define "ironrag.startupJobName" -}}
{{- printf "%s-startup-r%d" (include "ironrag.fullname" .) .Release.Revision | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "ironrag.startupDependencyWaitInitContainer" -}}
{{- if eq .Values.dependencies.postgres.mode "bundled" }}
- name: wait-for-bundled-postgres
  image: "{{ .Values.startup.wait.image.repository }}:{{ .Values.startup.wait.image.tag }}"
  imagePullPolicy: {{ .Values.startup.wait.image.pullPolicy }}
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
  command:
    - kubectl
  args:
    - wait
    - --namespace={{ .Release.Namespace }}
    - --for=condition=available
    - --timeout={{ .Values.startup.wait.timeoutSeconds }}s
    - deployment/{{ include "ironrag.fullname" . }}-redis
{{- end }}
{{- if eq .Values.dependencies.arangodb.mode "bundled" }}
- name: wait-for-bundled-arangodb
  image: "{{ .Values.startup.wait.image.repository }}:{{ .Values.startup.wait.image.tag }}"
  imagePullPolicy: {{ .Values.startup.wait.image.pullPolicy }}
  command:
    - kubectl
  args:
    - wait
    - --namespace={{ .Release.Namespace }}
    - --for=condition=available
    - --timeout={{ .Values.startup.wait.timeoutSeconds }}s
    - deployment/{{ include "ironrag.fullname" . }}-arangodb
{{- end }}
{{- if and (eq .Values.storage.provider "s3") (eq .Values.storage.s3.mode "bundled") }}
- name: wait-for-bundled-s4core
  image: "{{ .Values.startup.wait.image.repository }}:{{ .Values.startup.wait.image.tag }}"
  imagePullPolicy: {{ .Values.startup.wait.image.pullPolicy }}
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
{{- if and (eq .Values.dependencies.arangodb.mode "external") (empty .Values.runtimeSecret.existingSecret) (and (empty .Values.dependencies.arangodb.url) (empty .Values.dependencies.arangodb.host)) -}}
{{- fail "dependencies.arangodb.url or dependencies.arangodb.host is required when dependencies.arangodb.mode=external and runtimeSecret.existingSecret is empty" -}}
{{- end -}}
{{- if and (eq .Values.storage.provider "s3") (eq .Values.storage.s3.mode "external") (empty .Values.storage.s3.bucket) -}}
{{- fail "storage.s3.bucket is required when storage.provider=s3" -}}
{{- end -}}
{{- end -}}
