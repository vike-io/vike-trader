//! The pinned models: which repository, which COMMIT, and the sha256 and size of every file a model
//! needs. A pin is a supply-chain decision, like `vike_ml_training::pins`: the revision is a 40-hex commit (a
//! branch name could move under the same words), and the digest is the check [`crate::fetch`] makes
//! before a byte reaches the models folder.
//!
//! # Bumping a pin
//!
//! Read the new commit's file list from the hub (`https://huggingface.co/api/models/<repository>/revision/<commit>?blobs=true`
//! gives each large file's `lfs.sha256` and `size`; a small file's sha256 is taken from its bytes),
//! move the revision and every digest together, then run the real-weights test
//! (`crates/vike-nlp/tests/pinned_models.rs`) on a lane. A changed score for the same text is a
//! FINDING to write in the commit message, never a test to re-baseline quietly.
//!
//! Both models share the BERT uncased WordPiece vocabulary (the two `vocab.txt` files are the same
//! bytes) and the `bert` encoder shape `candle-transformers` loads.

/// What a model computes, and so which head [`crate::sentiment`] or [`crate::embedding`] loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTask {
    /// A sequence classifier: the BERT pooler over `[CLS]`, then a linear layer over the classes the
    /// model's `config.json` names in `id2label`.
    Sentiment,
    /// A sentence encoder: the mean of the last hidden states over the real tokens, L2-normalized.
    Embedding,
}

/// One file of a pinned model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedFile {
    /// The path inside the repository at [`PinnedModel::revision`], `/`-separated.
    pub path: &'static str,
    /// Lowercase hex sha256 of the whole file.
    pub sha256: &'static str,
    /// The file's size in bytes, checked before the (slower) digest.
    pub size: u64,
}

/// One model at one pinned revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedModel {
    /// The name the command line and the models folder use.
    pub name: &'static str,
    /// The Hugging Face repository (`owner/name`).
    pub repository: &'static str,
    /// The repository COMMIT the files are read at: 40 lowercase hex characters.
    pub revision: &'static str,
    /// The weights' licence as the repository states it. Fetched by the user onto the user's own
    /// machine and never redistributed by this project.
    pub license: &'static str,
    /// Which head the model carries.
    pub task: ModelTask,
    /// The longest input in tokens, the two special tokens included; a longer text is cut at the end.
    pub max_tokens: usize,
    /// Every file the loader reads: `config.json`, `vocab.txt` and `model.safetensors`.
    pub files: &'static [PinnedFile],
}

/// The file names the loader reads inside a model folder.
pub const CONFIG_FILE: &str = "config.json";
/// The WordPiece vocabulary, one token per line.
pub const VOCABULARY_FILE: &str = "vocab.txt";
/// The weights, in the safetensors format (never a pickled `pytorch_model.bin`).
pub const WEIGHTS_FILE: &str = "model.safetensors";

/// The BERT uncased vocabulary both models ship (identical bytes in both repositories).
const BERT_UNCASED_VOCABULARY: PinnedFile = PinnedFile {
    path: VOCABULARY_FILE,
    sha256: "07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3",
    size: 231_508,
};

/// FinBERT (Araci 2019, ProsusAI): BERT-base fine-tuned on the Financial PhraseBank; classes
/// `positive`, `negative`, `neutral`. 512 tokens, the BERT-base maximum.
pub const SENTIMENT_MODEL: PinnedModel = PinnedModel {
    name: "finbert",
    repository: "ProsusAI/finbert",
    revision: "0bc2ac10dda6929bcb1f77ccc43c7ec5c424f031",
    license: "cc-by-nc-sa-4.0",
    task: ModelTask::Sentiment,
    max_tokens: 512,
    files: &[
        PinnedFile {
            path: CONFIG_FILE,
            sha256: "f6449ddda85eb726207a40be59c0cd3bd4b142ccb27298d5e45f9ae3396b1abe",
            size: 758,
        },
        BERT_UNCASED_VOCABULARY,
        PinnedFile {
            path: WEIGHTS_FILE,
            sha256: "e5897858ff819aad7629b96ce521ae5477952d03634ef5dd30ed2d76357a9f00",
            size: 437_965_908,
        },
    ],
};

/// all-MiniLM-L6-v2 (sentence-transformers): a 6-layer, 384-wide encoder trained for sentence
/// similarity; mean pooling then L2 normalization, as its `modules.json` declares. 256 tokens, its
/// `sentence_bert_config.json`'s `max_seq_length`.
pub const EMBEDDING_MODEL: PinnedModel = PinnedModel {
    name: "all-minilm-l6-v2",
    repository: "sentence-transformers/all-MiniLM-L6-v2",
    revision: "1110a243fdf4706b3f48f1d95db1a4f5529b4d41",
    license: "apache-2.0",
    task: ModelTask::Embedding,
    max_tokens: 256,
    files: &[
        PinnedFile {
            path: CONFIG_FILE,
            sha256: "953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41",
            size: 612,
        },
        BERT_UNCASED_VOCABULARY,
        PinnedFile {
            path: WEIGHTS_FILE,
            sha256: "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db",
            size: 90_868_376,
        },
    ],
};

/// Every pinned model, in the order `vike-nlp fetch` downloads them.
pub const PINNED_MODELS: [PinnedModel; 2] = [SENTIMENT_MODEL, EMBEDDING_MODEL];

/// The pinned model named `name`, if any.
#[must_use]
pub fn pinned(name: &str) -> Option<&'static PinnedModel> {
    PINNED_MODELS.iter().find(|m| m.name == name)
}

impl PinnedModel {
    /// Where this model's files live under a models folder: `<models_dir>/<name>/<revision>`. A new
    /// revision is a new folder, so a bump never overwrites the files an older run used.
    #[must_use]
    pub fn folder_in(&self, models_dir: &std::path::Path) -> std::path::PathBuf {
        models_dir.join(self.name).join(self.revision)
    }

    /// The download address of one file at the pinned revision.
    #[must_use]
    pub fn file_url(&self, file: &PinnedFile) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repository, self.revision, file.path
        )
    }

    /// `name@revision`: how a stored number names the model that made it.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}@{}", self.name, self.revision)
    }
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
