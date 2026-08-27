mod background_tasks;
mod bundle_index;
mod embedded;
mod launchd;
mod login_items;
mod metadata;

use crate::{
    PlatformCancellation, PlatformError, PlatformErrorCode, PlatformResult,
    PlatformStartupChangeRequest, PlatformStartupChangeResult, PlatformStartupControlCapability,
    PlatformStartupCoverageReason, PlatformStartupSourceResult,
};

pub(super) fn scan(
    cancellation: &PlatformCancellation,
) -> PlatformResult<Vec<PlatformStartupSourceResult>> {
    let launchd_cancellation = cancellation.clone();
    let embedded_cancellation = cancellation.clone();
    let login_cancellation = cancellation.clone();
    let background_cancellation = cancellation.clone();
    let (mut results, embedded_result, login_result, background_result) =
        std::thread::scope(|scope| {
            let launchd = scope.spawn(|| launchd::scan(&launchd_cancellation));
            let embedded = scope.spawn(|| embedded::scan(&embedded_cancellation));
            let login_items = scope.spawn(|| login_items::scan(&login_cancellation));
            let background_tasks = scope.spawn(|| background_tasks::scan(&background_cancellation));
            (
                launchd.join().unwrap_or_else(|_| {
                    vec![PlatformStartupSourceResult::unavailable(
                        "macos.launchd",
                        true,
                        PlatformStartupCoverageReason::InvalidData,
                    )]
                }),
                embedded.join().unwrap_or_else(|_| {
                    PlatformStartupSourceResult::unavailable(
                        "macos.embedded_items",
                        true,
                        PlatformStartupCoverageReason::InvalidData,
                    )
                }),
                login_items.join().unwrap_or_else(|_| {
                    PlatformStartupSourceResult::unavailable(
                        "macos.login_items",
                        true,
                        PlatformStartupCoverageReason::InvalidData,
                    )
                }),
                background_tasks.join().unwrap_or_else(|_| {
                    PlatformStartupSourceResult::unavailable(
                        "macos.background_tasks",
                        false,
                        PlatformStartupCoverageReason::InvalidData,
                    )
                }),
            )
        });
    results.push(embedded_result);
    results.push(login_result);
    results.push(background_result);
    Ok(results)
}

pub(super) fn change(
    request: &PlatformStartupChangeRequest,
    authorization_prompt: Option<&str>,
) -> PlatformResult<PlatformStartupChangeResult> {
    if request.expected_artifact.control_capability
        == PlatformStartupControlCapability::ElevationRequired
    {
        return crate::startup_helper::change_with_privileges(request, authorization_prompt);
    }
    change_direct(request)
}

pub(super) fn change_many(
    requests: &[PlatformStartupChangeRequest],
    authorization_prompt: Option<&str>,
) -> PlatformResult<Vec<PlatformResult<PlatformStartupChangeResult>>> {
    let privileged = requests
        .iter()
        .filter(|request| {
            request.expected_artifact.control_capability
                == PlatformStartupControlCapability::ElevationRequired
        })
        .collect::<Vec<_>>();
    let mut privileged_results = if privileged.is_empty() {
        Vec::new()
    } else {
        crate::startup_helper::change_many_with_privileges(&privileged, authorization_prompt)?
    }
    .into_iter();

    Ok(requests
        .iter()
        .map(|request| {
            if request.expected_artifact.control_capability
                == PlatformStartupControlCapability::ElevationRequired
            {
                privileged_results.next().unwrap_or_else(|| {
                    Err(PlatformError::new(
                        PlatformErrorCode::InvalidData,
                        "startup helper returned too few batch results",
                    ))
                })
            } else {
                change_direct(request)
            }
        })
        .collect())
}

fn change_direct(
    request: &PlatformStartupChangeRequest,
) -> PlatformResult<PlatformStartupChangeResult> {
    match request.source_id.as_str() {
        "macos.launchd.user_agents" => launchd::change(request),
        "macos.background_tasks" => background_tasks::change(request),
        _ => Err(PlatformError::new(
            PlatformErrorCode::Unsupported,
            "startup source does not support configured-state changes",
        )),
    }
}

pub(super) fn helper_change_many(
    requests: &[crate::startup_helper::StartupHelperChangeRequest],
    interactive_user_id: u32,
) -> Vec<PlatformResult<PlatformStartupChangeResult>> {
    let cancellation = PlatformCancellation::new(|| false);
    let bundle_index = bundle_index::BundleIndex::discover();
    let results = launchd::scan_with_bundle_index(&cancellation, &bundle_index);
    requests
        .iter()
        .map(|request| {
            helper_change_from_snapshot(request, &results, interactive_user_id, &bundle_index)
        })
        .collect()
}

fn helper_change_from_snapshot(
    request: &crate::startup_helper::StartupHelperChangeRequest,
    results: &[PlatformStartupSourceResult],
    interactive_user_id: u32,
    bundle_index: &bundle_index::BundleIndex,
) -> PlatformResult<PlatformStartupChangeResult> {
    if !matches!(
        request.source_id.as_str(),
        "macos.launchd.local_agents" | "macos.launchd.local_daemons"
    ) {
        return Err(PlatformError::new(
            PlatformErrorCode::Unsupported,
            "startup helper source is not allowlisted",
        ));
    }
    let artifact = results
        .iter()
        .find(|source| source.source_id == request.source_id)
        .and_then(|source| {
            source
                .items
                .iter()
                .find(|artifact| artifact.provider_item_id == request.provider_item_id)
        })
        .cloned()
        .ok_or_else(|| PlatformError::item_changed("startup helper target no longer exists"))?;
    if artifact.control_capability != PlatformStartupControlCapability::ElevationRequired
        || crate::startup_helper::artifact_digest(&artifact) != request.expected_artifact_digest
    {
        return Err(PlatformError::item_changed(
            "startup helper target changed after preflight",
        ));
    }
    launchd::privileged_change_with_bundle_index(
        &PlatformStartupChangeRequest {
            provider_item_id: request.provider_item_id.clone(),
            source_id: request.source_id.clone(),
            expected_artifact: artifact,
            desired_state: request.desired_state,
        },
        interactive_user_id,
        bundle_index,
    )
}
