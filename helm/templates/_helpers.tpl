{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "qebbeq.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Expand the name of the chart.
*/}}
{{- define "qebbeq-controller.name" -}}
{{- printf "%s-controller" .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "qebbeq-controller.fullname" -}}
{{- if contains .Chart.Name .Release.Name }}
{{- printf "%s-controller" .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s-controller" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{/*
Common Controller labels
*/}}
{{- define "qebbeq-controller.labels" -}}
helm.sh/chart: {{ include "qebbeq.chart" . }}
{{ include "qebbeq-controller.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Controller Selector labels
*/}}
{{- define "qebbeq-controller.selectorLabels" -}}
app.kubernetes.io/name: {{ include "qebbeq-controller.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: controller
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "qebbeq-controller.serviceAccountName" -}}
{{- if .Values.controller.serviceAccount.create }}
{{- default (include "qebbeq-controller.fullname" .) .Values.controller.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.controller.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Expand the name of the chart.
*/}}
{{- define "qebbeq-registry.name" -}}
{{- printf "%s-registry" .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "qebbeq-registry.fullname" -}}
{{- if contains .Chart.Name .Release.Name }}
{{- printf "%s-registry" .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s-registry" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{/*
Common Registry labels
*/}}
{{- define "qebbeq-registry.labels" -}}
helm.sh/chart: {{ include "qebbeq.chart" . }}
{{ include "qebbeq-registry.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Registry Selector labels
*/}}
{{- define "qebbeq-registry.selectorLabels" -}}
app.kubernetes.io/name: {{ include "qebbeq-registry.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: registry
app.kubernetes.io/part-of: qebbeq
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "qebbeq-registry.serviceAccountName" -}}
{{- if .Values.registry.serviceAccount.create }}
{{- default (include "qebbeq-registry.fullname" .) .Values.registry.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.registry.serviceAccount.name }}
{{- end }}
{{- end }}
