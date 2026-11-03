//! Directory-view file loading and cross-file extracted chromatograms.

use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use anyhow::Result;

use crate::{
    explore_data::{self, ChromPoint, Dataset},
    explore_features::FeaturePanel,
    explore_files::MsFile,
    feature_list_view::FeatureList,
    inspect::Chromatogram,
};

pub(crate) struct LoadedFile {
    pub data: Dataset,
    pub order: Vec<usize>,
    pub feature_panel: Option<FeaturePanel>,
}

pub(crate) fn load(entry: &MsFile, mode: Chromatogram) -> Result<LoadedFile> {
    let data = explore_data::load(&entry.raw, entry.audit.as_deref(), mode)?;
    let feature_panel = entry
        .audit
        .as_deref()
        .map(|audit| FeatureList::load(audit, entry.feature_list.as_deref(), &data.features))
        .transpose()?
        .flatten()
        .map(FeaturePanel::new);
    let mut order: Vec<usize> = (0..data.features.len()).collect();
    order.sort_by(|&a, &b| {
        data.features[a]
            .rt_seconds
            .total_cmp(&data.features[b].rt_seconds)
            .then_with(|| data.features[a].id.cmp(&data.features[b].id))
    });
    Ok(LoadedFile {
        data,
        order,
        feature_panel,
    })
}

pub(crate) enum LoadOperation {
    Dataset(Chromatogram),
    Chromatogram(Chromatogram),
}

pub(crate) struct FileRequest {
    pub index: usize,
    pub generation: u64,
    pub operation: LoadOperation,
}

pub(crate) enum LoadedResult {
    Dataset(LoadedFile),
    Chromatogram(Vec<ChromPoint>),
}

pub(crate) struct FileReply {
    pub index: usize,
    pub generation: u64,
    pub result: std::result::Result<LoadedResult, String>,
}

pub(crate) struct FileLoader {
    requests: Sender<FileRequest>,
    replies: Receiver<FileReply>,
}

impl FileLoader {
    pub fn start(files: Arc<[MsFile]>) -> Self {
        let (requests, incoming) = mpsc::channel::<FileRequest>();
        let (outgoing, replies) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(request) = incoming.recv() {
                let file = &files[request.index];
                let result = match request.operation {
                    LoadOperation::Dataset(mode) => load(file, mode).map(LoadedResult::Dataset),
                    LoadOperation::Chromatogram(mode) => {
                        explore_data::chromatogram(&file.raw, mode).map(LoadedResult::Chromatogram)
                    }
                }
                .map_err(|error| format!("{error:#}"));
                if outgoing
                    .send(FileReply {
                        index: request.index,
                        generation: request.generation,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self { requests, replies }
    }

    pub fn request(&self, request: FileRequest) -> bool {
        self.requests.send(request).is_ok()
    }

    pub fn poll(&self) -> Option<FileReply> {
        self.replies.try_recv().ok()
    }
}
