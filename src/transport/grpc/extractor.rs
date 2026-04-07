#[cfg(feature = "opentelemetry")]
use opentelemetry::Context;
#[cfg(feature = "opentelemetry")]
use opentelemetry::global;
#[cfg(feature = "opentelemetry")]
use opentelemetry::propagation::Extractor;
#[cfg(feature = "opentelemetry")]
use tonic::metadata::MetadataMap;

#[cfg(feature = "opentelemetry")]
struct MetadataMapExtractor<'a>(&'a MetadataMap);

#[cfg(feature = "opentelemetry")]
impl Extractor for MetadataMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(|key| match key {
                tonic::metadata::KeyRef::Ascii(key) => key.as_str(),
                tonic::metadata::KeyRef::Binary(key) => key.as_str(),
            })
            .collect()
    }
}

#[cfg(feature = "opentelemetry")]
pub(crate) fn extract_context(metadata: &MetadataMap) -> Context {
    global::get_text_map_propagator(|propagator| propagator.extract(&MetadataMapExtractor(metadata)))
}
