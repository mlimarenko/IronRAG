;(function () {
  if (!window.location.hash.includes('figmacapture=')) {
    return
  }

  var script = document.createElement('script')
  script.src = 'https://mcp.figma.com/mcp/html-to-design/capture.js'
  script.async = true
  document.head.appendChild(script)
})()
