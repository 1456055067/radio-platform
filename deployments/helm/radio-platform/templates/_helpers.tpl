{{- define "rp.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "rp.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{- define "rp.selector" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: gateway
{{- end -}}

{{- define "rp.image" -}}
{{- if .Values.image.digest -}}
{{ .Values.image.repository }}@{{ .Values.image.digest }}
{{- else -}}
{{ .Values.image.repository }}:{{ .Values.image.tag | default .Chart.AppVersion }}
{{- end -}}
{{- end -}}

{{/* gateway.toml: chart-controlled tables, then the user TOML from values.config. */}}
{{- define "rp.config" -}}
{{- $fixed := dict
  "http" (dict "bind" "0.0.0.0:8443" "tls" (dict "cert" "/etc/radio/tls/tls.crt" "key" "/etc/radio/tls/tls.key"))
  "agents" (dict "bind" "0.0.0.0:9443" "cert" "/etc/radio/tls/tls.crt" "key" "/etc/radio/tls/tls.key" "client_ca" "/etc/radio/agent-ca/ca.crt")
  "database" (dict "backend" "postgres" "max_connections" (int .Values.database.maxConnections))
  "cluster" (dict "enabled" (gt (int .Values.replicaCount) 1))
-}}
{{- if .Values.database.insecureTransport -}}
{{- $_ := set $fixed.database "insecure_transport" true -}}
{{- end -}}
{{- if .Values.secrets.agentCrl -}}
{{- $_ := set $fixed.agents "client_crl" "/etc/radio/agent-ca/ca.crl" -}}
{{- end -}}
{{- if .Values.ui.enabled -}}
{{- $_ := set $fixed.http "ui" (dict "dir" "/usr/share/radio-platform/ui" "auth" (dict "mode" "oidc" "authority" .Values.ui.authority "client_id" .Values.ui.clientId "scope" .Values.ui.scope)) -}}
{{- end -}}
{{- if .Values.metrics.enabled -}}
{{- $_ := set $fixed "metrics" (dict "bind" (printf "0.0.0.0:%d" (int .Values.metrics.port))) -}}
{{- end -}}
{{- if .Values.media.enabled -}}
{{- $_ := set $fixed "media" (dict "udp_bind" (printf "0.0.0.0:%d" (int .Values.media.udpPort)) "public" .Values.media.publicAddress) -}}
{{- end -}}
{{- toToml $fixed }}
# --- values.config ---
{{ .Values.config }}
{{- end -}}
