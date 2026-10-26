{{- define "kausa.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "kausa.fullname" -}}
{{- if contains .Chart.Name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{- define "kausa.selectorLabels" -}}
app.kubernetes.io/name: {{ include "kausa.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "kausa.labels" -}}
{{ include "kausa.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "kausa.serviceAccountName" -}}
{{- default (include "kausa.fullname" .) .Values.serviceAccount.name }}
{{- end }}

{{- define "kausa.image" -}}
{{- if .Values.image.digest }}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) }}
{{- end }}
{{- end }}

{{/* kausa explains incidents (and so may read logs) with an endpoint or in dry-run. */}}
{{- define "kausa.explains" -}}
{{- if or .Values.model.endpoint .Values.dryRun }}true{{ end }}
{{- end }}

{{/* kausa writes Events with an endpoint, outside dry-run, unless events is false. */}}
{{- define "kausa.writesEvents" -}}
{{- if and .Values.model.endpoint .Values.events (not .Values.dryRun) }}true{{ end }}
{{- end }}

{{- define "kausa.validate" -}}
{{- if and .Values.model.endpoint (not .Values.model.name) }}
{{- fail "model.name is required with model.endpoint" }}
{{- end }}
{{- if and (hasPrefix "http://" .Values.model.endpoint) (not .Values.model.allowHttp) }}
{{- fail "model.endpoint is plain http://: set model.allowHttp=true (only for an endpoint in the cluster or on the same private network)" }}
{{- end }}
{{- if and .Values.networkPolicy.enabled (not .Values.networkPolicy.apiServer.cidrs) }}
{{- fail "networkPolicy.apiServer.cidrs is required with networkPolicy.enabled: without it kausa cannot reach the API server" }}
{{- end }}
{{- if and .Values.networkPolicy.enabled .Values.model.endpoint (not .Values.dryRun) (not .Values.networkPolicy.endpoint.to) }}
{{- fail "networkPolicy.endpoint.to is required with networkPolicy.enabled and model.endpoint: without it kausa cannot reach the model" }}
{{- end }}
{{- end }}
