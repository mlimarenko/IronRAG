FROM node@sha256:725aeba2364a9b16beae49e180d83bd597dbd0b15c47f1f28875c290bfd255b9 AS builder
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci

ARG APP_VERSION
# Browser tracing is opt-in at image build time. Supply an operator-controlled
# OTLP/HTTP traces endpoint to enable it; an ordinary image exports nothing.
ARG VITE_OTEL_EXPORTER_OTLP_ENDPOINT=
ARG VITE_OTEL_SERVICE_NAME
ARG VITE_OTEL_SERVICE_VERSION
ARG VITE_OTEL_DEPLOYMENT_ENVIRONMENT
ARG VITE_ENABLE_QUERY_DEVTOOLS=false
ENV VITE_APP_VERSION=${APP_VERSION}
ENV VITE_OTEL_EXPORTER_OTLP_ENDPOINT=${VITE_OTEL_EXPORTER_OTLP_ENDPOINT}
ENV VITE_OTEL_SERVICE_NAME=${VITE_OTEL_SERVICE_NAME}
ENV VITE_OTEL_SERVICE_VERSION=${VITE_OTEL_SERVICE_VERSION}
ENV VITE_OTEL_DEPLOYMENT_ENVIRONMENT=${VITE_OTEL_DEPLOYMENT_ENVIRONMENT}
ENV VITE_ENABLE_QUERY_DEVTOOLS=${VITE_ENABLE_QUERY_DEVTOOLS}
COPY . .
RUN ./node_modules/.bin/vite build

FROM nginxinc/nginx-unprivileged@sha256:b3f2436575bd5be7386518084d842dac414ab4962712afa31e99e0942a56e3b2
USER root
COPY --from=builder /app/dist /usr/share/nginx/html
RUN apk add --no-cache --upgrade \
        c-ares=1.34.8-r0 \
        libexpat=2.8.2-r0 \
    && rm /etc/nginx/conf.d/default.conf \
    && mkdir -p /etc/nginx/snippets /etc/nginx/templates-available /etc/nginx/templates \
    && chown -R nginx:root /etc/nginx
COPY security-headers.conf /etc/nginx/snippets/ironrag-security-headers.conf
COPY nginx.conf.template /etc/nginx/templates/default.conf.template
COPY nginx-tls.conf.template /etc/nginx/templates-available/ironrag-tls.conf.template
COPY docker-entrypoint.d/05-ironrag-tls-defaults.envsh /docker-entrypoint.d/05-ironrag-tls-defaults.envsh
COPY docker-entrypoint.d/10-enable-ironrag-tls.sh /docker-entrypoint.d/10-enable-ironrag-tls.sh
RUN chmod +x /docker-entrypoint.d/05-ironrag-tls-defaults.envsh /docker-entrypoint.d/10-enable-ironrag-tls.sh
# IRONRAG_API_UPSTREAM defaults to http://127.0.0.1:8080 via this ENV instruction.
# Override at runtime to point nginx reverse-proxy at the actual API backend.
ENV IRONRAG_API_UPSTREAM=http://127.0.0.1:8080
USER nginx
EXPOSE 8080 8443/tcp 8443/udp
CMD ["nginx", "-g", "daemon off;"]
