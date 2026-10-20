/// Concrete capability implementations
pub mod search;
pub mod vector;

pub use search::{SearchCapability, SearchDocument, SearchResult};
pub use vector::{
    DemoEmbeddingProvider, EmbeddingProvider, VectorCapability, VectorDocument, VectorSearchResult,
};
