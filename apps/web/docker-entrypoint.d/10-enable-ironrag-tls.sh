#!/bin/sh
set -eu

tls_template=/etc/nginx/templates-available/ironrag-tls.conf.template
tls_render_template=/etc/nginx/templates/ironrag-tls.conf.template
tls_rendered_conf=/etc/nginx/conf.d/ironrag-tls.conf

cert=${IRONRAG_NGINX_TLS_CERTIFICATE_PATH:-/etc/nginx/tls/tls.crt}
key=${IRONRAG_NGINX_TLS_CERTIFICATE_KEY_PATH:-/etc/nginx/tls/tls.key}

if [ -f "$cert" ] && [ -f "$key" ]; then
    cp "$tls_template" "$tls_render_template"
    echo "ironrag nginx: TLS certificate found; enabling HTTPS, HTTP/2, and HTTP/3"
else
    rm -f "$tls_render_template" "$tls_rendered_conf"
    echo "ironrag nginx: TLS certificate not found; serving HTTP only"
fi
