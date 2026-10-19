use std::str::FromStr;

pub(super) const TEXT_LIKE_EXTENSIONS: &[&str] = &[
    // Text and markup
    "txt",
    "md",
    "markdown",
    "json",
    "jsonl",
    "ndjson",
    "yaml",
    "yml",
    "xml",
    "svg",
    "log",
    "rst",
    "toml",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
    // Web
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "css",
    "scss",
    "less",
    "sass",
    "vue",
    "svelte",
    // Systems
    "rs",
    "go",
    "c",
    "h",
    "cpp",
    "cc",
    "cxx",
    "hpp",
    "hh",
    // JVM
    "java",
    "kt",
    "kts",
    "scala",
    "groovy",
    "gradle",
    // .NET
    "cs",
    "fs",
    "vb",
    "csproj",
    "sln",
    // Scripting
    "py",
    "rb",
    "php",
    "lua",
    "pl",
    "pm",
    "r",
    "jl",
    // Mobile
    "swift",
    "dart",
    "m",
    "mm",
    // Functional
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "cljs",
    "elm",
    // Shell and infra
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "tf",
    "hcl",
    "dockerfile",
    "vagrantfile",
    // Data and query
    "sql",
    "graphql",
    "gql",
    "proto",
    "avsc",
    // Build and config
    "makefile",
    "cmake",
    "ninja",
    "bazel",
    "buck",
];

pub(super) const HTML_EXTENSIONS: &[&str] = &["html", "htm"];

pub(super) const IMAGE_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"];

pub(super) const DOCX_EXTENSIONS: &[&str] = &["docx"];

pub(super) const PPTX_EXTENSIONS: &[&str] = &["pptx"];

pub(super) const HTML_MIME_TYPES: &[&str] = &["text/html", "application/xhtml+xml"];

pub(super) const TEXT_LIKE_MIME_TYPES: &[&str] = &[
    "application/json",
    "application/jsonl",
    "application/ndjson",
    "application/x-jsonlines",
    "application/x-ndjson",
    "application/yaml",
    "application/x-yaml",
    "application/toml",
    "application/xml",
    "text/xml",
    "image/svg+xml",
];

pub(super) const IMAGE_MIME_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/bmp", "image/webp", "image/tiff"];

pub(super) const DOCX_MIME_TYPES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.wordprocessingml.document"];

pub(super) const PPTX_MIME_TYPES: &[&str] =
    &["application/vnd.openxmlformats-officedocument.presentationml.presentation"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadFileKind {
    TextLike,
    Pdf,
    Image,
    Docx,
    Spreadsheet,
    Pptx,
    Binary,
}

impl UploadFileKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextLike => "text_like",
            Self::Pdf => "pdf",
            Self::Image => "image",
            Self::Docx => "docx",
            Self::Spreadsheet => "spreadsheet",
            Self::Pptx => "pptx",
            Self::Binary => "binary",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::TextLike => "Text",
            Self::Pdf => "PDF",
            Self::Image => "Image",
            Self::Docx => "DOCX",
            Self::Spreadsheet => "Spreadsheet",
            Self::Pptx => "PPTX",
            Self::Binary => "Binary",
        }
    }
}

impl FromStr for UploadFileKind {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "text_like" => Ok(Self::TextLike),
            "pdf" => Ok(Self::Pdf),
            "image" => Ok(Self::Image),
            "docx" => Ok(Self::Docx),
            "spreadsheet" => Ok(Self::Spreadsheet),
            "pptx" => Ok(Self::Pptx),
            "binary" => Ok(Self::Binary),
            _ => Err(()),
        }
    }
}

pub(super) fn lower_file_extension(file_name: Option<&str>) -> Option<String> {
    file_name
        .and_then(|value| std::path::Path::new(value).extension())
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
}
