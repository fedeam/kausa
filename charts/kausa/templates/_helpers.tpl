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

{{/*
The history map is read nil-safe: a release made before it (helm upgrade --reuse-values) or
--set history=null has none. The keys that default to something other than false default with
hasKey, never with default, which would turn a false into true.
*/}}

{{/* kausa writes the history with history.enabled, an endpoint and outside dry-run. */}}
{{- define "kausa.writesHistory" -}}
{{- $h := .Values.history | default dict }}
{{- if and $h.enabled .Values.model.endpoint (not .Values.dryRun) }}true{{ end }}
{{- end }}

{{/* history.days as a string, 7 when missing; kausa.validate checks it. */}}
{{- define "kausa.historyDays" -}}
{{- $h := .Values.history | default dict }}
{{- if hasKey $h "days" }}{{ toString $h.days }}{{ else }}7{{ end }}
{{- end }}

{{/* The chart installs the Explanation CRD while kausa writes the history, unless installCRD is
false (another release or a cluster administrator installs it). */}}
{{- define "kausa.installsCRD" -}}
{{- $h := .Values.history | default dict }}
{{- $install := true }}
{{- if hasKey $h "installCRD" }}{{ $install = $h.installCRD }}{{ end }}
{{- if and (include "kausa.writesHistory" .) $install }}true{{ end }}
{{- end }}

{{/*
The rules of the ClusterRole (rbac.scope=cluster) and of every Role (rbac.scope=namespaced): the
permissions in docs/reference/permissions.md, and nothing else. A later feature adds its rules
only here, so that both scopes grant the same.
*/}}
{{- define "kausa.rules" -}}
- apiGroups: [""]
  resources: ["pods"]
  verbs: ["get", "list", "watch"]
{{- if and .Values.logs (include "kausa.explains" .) }}
- apiGroups: [""]
  resources: ["pods/log"]
  verbs: ["get"]
{{- end }}
- apiGroups: ["events.k8s.io"]
  resources: ["events"]
  verbs: ["list", "watch"]
{{- if include "kausa.writesEvents" . }}
- apiGroups: ["events.k8s.io"]
  resources: ["events"]
  verbs: ["create", "patch"]
{{- end }}
- apiGroups: ["apps"]
  resources: ["replicasets"]
  verbs: ["get"]
- apiGroups: ["batch"]
  resources: ["jobs"]
  verbs: ["get"]
{{- if include "kausa.writesHistory" . }}
- apiGroups: ["kausa.fedeam.com"]
  resources: ["explanations"]
  verbs: ["list", "create", "patch", "delete"]
{{- end }}
{{- end }}

{{/*
Refuses to render with a reason. The rbac checks come first, in this order: the scope, the list
against the scope, every name, the first repeat, and the count. rbac is read nil-safe: a v0.1
release's stored values and --set rbac=null have no rbac map, and must reach the "not set"
message, not a nil-pointer error. history.days comes last, with the history on or off.
*/}}
{{- define "kausa.validate" -}}
{{- $rbac := .Values.rbac | default dict }}
{{- $scope := $rbac.scope | default "" | toString }}
{{- $namespaces := $rbac.namespaces | default list }}
{{- $max := 32 }}
{{- if not $scope }}
{{- fail "rbac.scope is not set: set rbac.scope=cluster to let kausa read every namespace, or rbac.scope=namespaced with rbac.namespaces" }}
{{- end }}
{{- if not (has $scope (list "cluster" "namespaced")) }}
{{- fail (printf "rbac.scope must be cluster or namespaced, not '%s'" $scope) }}
{{- end }}
{{- if and (eq $scope "namespaced") (not $namespaces) }}
{{- fail "rbac.scope=namespaced needs rbac.namespaces: the namespaces kausa may read" }}
{{- end }}
{{- if and (eq $scope "cluster") $namespaces }}
{{- fail "rbac.namespaces is set but rbac.scope is cluster, which grants every namespace: set rbac.scope=namespaced" }}
{{- end }}
{{- range $namespaces }}
{{- $name := toString . }}
{{- if not (regexMatch "^[a-z0-9]([-a-z0-9]{0,61}[a-z0-9])?$" $name) }}
{{- fail (printf "rbac.namespaces: '%s' is not a namespace name (lowercase letters, digits and '-', at most 63 characters)" $name) }}
{{- end }}
{{- end }}
{{- $seen := dict }}
{{- range $namespaces }}
{{- $name := toString . }}
{{- if hasKey $seen $name }}
{{- fail (printf "rbac.namespaces: '%s' is listed twice" $name) }}
{{- end }}
{{- $_ := set $seen $name true }}
{{- end }}
{{- if gt (len $namespaces) $max }}
{{- fail (printf "rbac.namespaces lists %d namespaces; kausa watches at most %d (use rbac.scope=cluster instead)" (len $namespaces) $max) }}
{{- end }}
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
{{- $days := include "kausa.historyDays" . }}
{{- if not (regexMatch "^([1-9]|[12][0-9]|30)$" $days) }}
{{- fail (printf "history.days must be a whole number of days from 1 to 30, not '%s'" $days) }}
{{- end }}
{{- end }}
