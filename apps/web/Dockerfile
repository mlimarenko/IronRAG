FROM node:24-alpine AS builder
WORKDIR /app
ARG APP_VERSION
# Browser traces default to the maintainers' collector (performance only, no
# document/query content). Build with --build-arg VITE_OTEL_EXPORTER_OTLP_ENDPOINT=
# (empty) to disable, or point at your own OTLP/HTTP traces endpoint.
ARG VITE_OTEL_EXPORTER_OTLP_ENDPOINT=https://otel.piping.space:4318/v1/traces
ARG VITE_OTEL_SERVICE_NAME
ARG VITE_OTEL_SERVICE_VERSION
ARG VITE_OTEL_DEPLOYMENT_ENVIRONMENT
ENV VITE_APP_VERSION=${APP_VERSION}
ENV VITE_OTEL_EXPORTER_OTLP_ENDPOINT=${VITE_OTEL_EXPORTER_OTLP_ENDPOINT}
ENV VITE_OTEL_SERVICE_NAME=${VITE_OTEL_SERVICE_NAME}
ENV VITE_OTEL_SERVICE_VERSION=${VITE_OTEL_SERVICE_VERSION}
ENV VITE_OTEL_DEPLOYMENT_ENVIRONMENT=${VITE_OTEL_DEPLOYMENT_ENVIRONMENT}
COPY package.json package-lock.json ./
RUN npm ci --legacy-peer-deps
COPY . .
RUN npx vite build

FROM nginx:1.30-alpine
COPY --from=builder /app/dist /usr/share/nginx/html
RUN rm /etc/nginx/conf.d/default.conf && mkdir -p /etc/nginx/snippets /etc/nginx/templates-available
COPY security-headers.conf /etc/nginx/snippets/ironrag-security-headers.conf
COPY nginx.conf.template /etc/nginx/templates/default.conf.template
COPY nginx-tls.conf.template /etc/nginx/templates-available/ironrag-tls.conf.template
COPY docker-entrypoint.d/05-ironrag-tls-defaults.envsh /docker-entrypoint.d/05-ironrag-tls-defaults.envsh
COPY docker-entrypoint.d/10-enable-ironrag-tls.sh /docker-entrypoint.d/10-enable-ironrag-tls.sh
RUN chmod +x /docker-entrypoint.d/05-ironrag-tls-defaults.envsh /docker-entrypoint.d/10-enable-ironrag-tls.sh
# IRONRAG_API_UPSTREAM defaults to http://127.0.0.1:8080 via this ENV instruction.
# Override at runtime to point nginx reverse-proxy at the actual API backend.
ENV IRONRAG_API_UPSTREAM=http://127.0.0.1:8080
EXPOSE 80 443/tcp 443/udp
CMD ["nginx", "-g", "daemon off;"]
