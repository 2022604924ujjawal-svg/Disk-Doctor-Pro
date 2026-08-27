mod models;
mod service;

pub use models::{
    ApplicationLeftoverOperationDetails, ApplicationUninstallApplicationDetails,
    ApplicationUninstallOperationDetails, CleanupOperationDetails, DeepCleanupOperationDetails,
    FileCleanupHistoryItem, FileCleanupHistoryItemStatus, FileCleanupOperationDetails,
    OperationCategory, OperationDetails, OperationOutcome, OperationRecord, StartupHistoryItem,
    StartupHistoryItemStatus, StartupHistoryState, StartupManagementOperationDetails,
    SystemOptimizationHistoryItem, SystemOptimizationHistoryItemStatus,
    SystemOptimizationOperationDetails, OPERATION_RECORD_SCHEMA_VERSION,
};
pub use service::HistoryService;
pub(crate) use service::{file_cleanup_record, summarize_deep_cleanup, FileCleanupHistoryCategory};
