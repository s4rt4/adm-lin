//! Kategori & auto-klasifikasi (plan §10). Ekstensi → kategori → subfolder
//! di bawah folder unduhan (mis. `%USERPROFILE%\Downloads\Compressed`).

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Category {
    Compressed,
    Documents,
    Music,
    Programs,
    Video,
    General,
}

impl Category {
    /// Tentukan kategori dari nama berkas (berdasar ekstensi).
    pub fn from_filename(name: &str) -> Category {
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "zip" | "7z" | "rar" | "gz" | "bz2" | "xz" | "tar" | "tgz" => Category::Compressed,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "epub" => {
                Category::Documents
            }
            "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" => Category::Music,
            "exe" | "msi" | "msix" | "bat" | "cmd" => Category::Programs,
            "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "m4v" => Category::Video,
            _ => Category::General,
        }
    }

    /// Subfolder default; `None` = langsung di folder unduhan (General).
    pub fn folder(self) -> Option<&'static str> {
        match self {
            Category::Compressed => Some("Compressed"),
            Category::Documents => Some("Documents"),
            Category::Music => Some("Music"),
            Category::Programs => Some("Programs"),
            Category::Video => Some("Video"),
            Category::General => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Compressed => "Compressed",
            Category::Documents => "Documents",
            Category::Music => "Music",
            Category::Programs => "Programs",
            Category::Video => "Video",
            Category::General => "General",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Category;

    #[test]
    fn classify() {
        assert_eq!(Category::from_filename("a.ZIP"), Category::Compressed);
        assert_eq!(Category::from_filename("doc.pdf"), Category::Documents);
        assert_eq!(Category::from_filename("song.mp3"), Category::Music);
        assert_eq!(Category::from_filename("setup.exe"), Category::Programs);
        assert_eq!(Category::from_filename("movie.mkv"), Category::Video);
        assert_eq!(Category::from_filename("noext"), Category::General);
        assert_eq!(Category::from_filename("data.bin"), Category::General);
    }

    #[test]
    fn folders() {
        assert_eq!(Category::Compressed.folder(), Some("Compressed"));
        assert_eq!(Category::General.folder(), None);
    }
}
