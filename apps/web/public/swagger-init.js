SwaggerUIBundle({
  url: '/v1/openapi/ironrag.openapi.yaml',
  dom_id: '#swagger-ui',
  docExpansion: 'list',
  defaultModelsExpandDepth: -1,
  requestInterceptor: function (req) {
    req.credentials = 'include'
    return req
  },
  presets: [SwaggerUIBundle.presets.apis],
  layout: 'BaseLayout',
})
