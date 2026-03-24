export const SUPPORTED_FORMATS = [
  {
    fileName: "sample.txt",
    mimeType: "text/plain",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "full",
  },
  {
    fileName: "sample.md",
    mimeType: "text/markdown",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "full",
  },
  {
    fileName: "sample.csv",
    mimeType: "text/csv",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "full",
  },
  {
    fileName: "sample.json",
    mimeType: "application/json",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "full",
  },
  {
    fileName: "sample.html",
    mimeType: "text/html",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "full",
  },
  {
    fileName: "sample.rtf",
    mimeType: "application/rtf",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "full",
  },
  {
    fileName: "sample.docx",
    mimeType: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "full",
  },
  {
    fileName: "sample.pdf",
    mimeType: "application/pdf",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "vision",
  },
  {
    fileName: "sample.png",
    mimeType: "image/png",
    expectedReadiness: { textReady: true, vectorReady: true, graphReady: true },
    expectedExtraction: "vision",
  },
];
