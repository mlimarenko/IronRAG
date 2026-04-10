FROM node:24-alpine AS builder
WORKDIR /app
ARG APP_VERSION
ENV VITE_APP_VERSION=${APP_VERSION}
COPY package.json package-lock.json ./
RUN npm ci --legacy-peer-deps
COPY . .
RUN npx vite build

FROM nginx:1.28-alpine
COPY --from=builder /app/dist /usr/share/nginx/html
RUN rm /etc/nginx/conf.d/default.conf
COPY nginx.conf.template /etc/nginx/templates/default.conf.template
# RUSTRAG_API_UPSTREAM defaults to http://127.0.0.1:8080 via this ENV instruction.
# Override at runtime to point nginx reverse-proxy at the actual API backend.
ENV RUSTRAG_API_UPSTREAM=http://127.0.0.1:8080
EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]
