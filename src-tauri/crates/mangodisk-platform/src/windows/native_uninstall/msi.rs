use std::{collections::BTreeSet, iter, ptr};

use windows_sys::Win32::{
    Foundation::{
        ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, ERROR_SUCCESS_REBOOT_INITIATED,
        ERROR_SUCCESS_REBOOT_REQUIRED,
    },
    System::ApplicationInstallationAndServicing::{
        MsiConfigureProductExW, MsiEnumProductsExW, MsiQueryProductStateW, INSTALLLEVEL_DEFAULT,
        INSTALLSTATE_ABSENT, INSTALLSTATE_ADVERTISED, INSTALLSTATE_BROKEN, INSTALLSTATE_DEFAULT,
        INSTALLSTATE_SOURCEABSENT, INSTALLSTATE_UNKNOWN, MSIINSTALLCONTEXT_ALL,
        MSIINSTALLCONTEXT_MACHINE, MSIINSTALLCONTEXT_USERMANAGED, MSIINSTALLCONTEXT_USERUNMANAGED,
    },
};

use crate::{
    ApplicationInstallScope, ApplicationUninstallExecutionOutcome,
    ApplicationUninstallPlatformError, ApplicationUninstallRegistrationState,
};

use super::system_directory_path;

const PRODUCT_CODE_BUFFER_LEN: usize = 39;

pub(super) fn install_scope(
    product_code: &str,
) -> Result<Option<ApplicationInstallScope>, ApplicationUninstallPlatformError> {
    let product_code = wide_string(product_code);
    let mut scopes = BTreeSet::new();
    let mut index = 0_u32;
    loop {
        let mut installed_product_code = [0_u16; PRODUCT_CODE_BUFFER_LEN];
        let mut installed_context = 0_i32;
        let status = unsafe {
            MsiEnumProductsExW(
                product_code.as_ptr(),
                ptr::null(),
                MSIINSTALLCONTEXT_ALL as u32,
                index,
                installed_product_code.as_mut_ptr(),
                &mut installed_context,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        match status {
            ERROR_SUCCESS => {
                let Some(scope) = scope_from_context(installed_context) else {
                    return Ok(None);
                };
                scopes.insert(scope);
                index = index.saturating_add(1);
            }
            ERROR_NO_MORE_ITEMS => break,
            code => return Err(ApplicationUninstallPlatformError::NativeFailure(code)),
        }
    }
    unique_scope(scopes)
}

pub(super) fn registration_state(
    product_code: &str,
    expected_scope: ApplicationInstallScope,
) -> Result<ApplicationUninstallRegistrationState, ApplicationUninstallPlatformError> {
    match install_scope(product_code)? {
        None => Ok(ApplicationUninstallRegistrationState::Absent),
        Some(scope) if scope != expected_scope => {
            Err(ApplicationUninstallPlatformError::RegistrationChanged)
        }
        Some(_) => Ok(product_state(product_code)),
    }
}

pub(super) fn execute(
    product_code: &str,
    scope: ApplicationInstallScope,
) -> Result<ApplicationUninstallExecutionOutcome, ApplicationUninstallPlatformError> {
    if registration_state(product_code, scope)? != ApplicationUninstallRegistrationState::Installed
    {
        return Err(ApplicationUninstallPlatformError::RegistrationChanged);
    }

    let outcome = if scope == ApplicationInstallScope::Machine {
        execute_elevated(product_code)?
    } else {
        execute_for_current_user(product_code)?
    };

    if registration_state(product_code, scope)? != ApplicationUninstallRegistrationState::Absent {
        return Err(ApplicationUninstallPlatformError::RegistrationChanged);
    }
    Ok(outcome)
}

fn execute_for_current_user(
    product_code: &str,
) -> Result<ApplicationUninstallExecutionOutcome, ApplicationUninstallPlatformError> {
    let product_code = wide_string(product_code);
    let command_line = wide_string("REBOOT=ReallySuppress");
    let status = unsafe {
        MsiConfigureProductExW(
            product_code.as_ptr(),
            INSTALLLEVEL_DEFAULT,
            INSTALLSTATE_ABSENT,
            command_line.as_ptr(),
        )
    };
    execution_outcome(status)
}

fn execute_elevated(
    product_code: &str,
) -> Result<ApplicationUninstallExecutionOutcome, ApplicationUninstallPlatformError> {
    if !valid_product_code(product_code) {
        return Err(ApplicationUninstallPlatformError::RegistrationChanged);
    }

    // The MSI elevation boundary accepts only a typed ProductCode and the
    // System32 executable. It shares UAC tracking but never trusts a registry path.
    let executable = system_directory_path()?.join("msiexec.exe");
    // MSI execution is advertised as silent. `/qn` also prevents an elevated
    // session from waiting for installer UI that the user cannot see.
    let arguments = format!("/x {product_code} /qn /norestart");
    let exit_code = super::execute_elevated_executable(&executable, &arguments, "windows_msi")?;
    execution_outcome(exit_code)
}

fn execution_outcome(
    status: u32,
) -> Result<ApplicationUninstallExecutionOutcome, ApplicationUninstallPlatformError> {
    match status {
        ERROR_SUCCESS => Ok(ApplicationUninstallExecutionOutcome::Completed),
        ERROR_SUCCESS_REBOOT_REQUIRED | ERROR_SUCCESS_REBOOT_INITIATED => {
            Ok(ApplicationUninstallExecutionOutcome::RestartRequired)
        }
        code => Err(ApplicationUninstallPlatformError::NativeFailure(code)),
    }
}

fn product_state(product_code: &str) -> ApplicationUninstallRegistrationState {
    let product_code = wide_string(product_code);
    match unsafe { MsiQueryProductStateW(product_code.as_ptr()) } {
        INSTALLSTATE_DEFAULT => ApplicationUninstallRegistrationState::Installed,
        INSTALLSTATE_UNKNOWN | INSTALLSTATE_ABSENT => ApplicationUninstallRegistrationState::Absent,
        INSTALLSTATE_ADVERTISED | INSTALLSTATE_BROKEN | INSTALLSTATE_SOURCEABSENT => {
            ApplicationUninstallRegistrationState::Incomplete
        }
        _ => ApplicationUninstallRegistrationState::Incomplete,
    }
}

fn unique_scope(
    scopes: BTreeSet<ApplicationInstallScope>,
) -> Result<Option<ApplicationInstallScope>, ApplicationUninstallPlatformError> {
    match scopes.len() {
        0 => Ok(None),
        1 => Ok(scopes.into_iter().next()),
        // Multiple contexts are ambiguous because the current catalog cannot
        // represent two native installation instances with one ProductCode.
        _ => Err(ApplicationUninstallPlatformError::RegistrationChanged),
    }
}

fn scope_from_context(context: i32) -> Option<ApplicationInstallScope> {
    match context {
        MSIINSTALLCONTEXT_USERMANAGED | MSIINSTALLCONTEXT_USERUNMANAGED => {
            Some(ApplicationInstallScope::CurrentUser)
        }
        MSIINSTALLCONTEXT_MACHINE => Some(ApplicationInstallScope::Machine),
        _ => None,
    }
}

fn valid_product_code(product_code: &str) -> bool {
    let bytes = product_code.as_bytes();
    bytes.len() == 38
        && bytes.first() == Some(&b'{')
        && bytes.last() == Some(&b'}')
        && [9, 14, 19, 24]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes[1..37]
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

fn wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_context_maps_to_explicit_scope() {
        assert_eq!(
            scope_from_context(MSIINSTALLCONTEXT_USERMANAGED),
            Some(ApplicationInstallScope::CurrentUser)
        );
        assert_eq!(
            scope_from_context(MSIINSTALLCONTEXT_USERUNMANAGED),
            Some(ApplicationInstallScope::CurrentUser)
        );
        assert_eq!(
            scope_from_context(MSIINSTALLCONTEXT_MACHINE),
            Some(ApplicationInstallScope::Machine)
        );
        assert_eq!(scope_from_context(0), None);
    }

    #[test]
    fn ambiguous_installer_context_is_not_reported_as_absent() {
        let mut scopes = BTreeSet::new();
        scopes.insert(ApplicationInstallScope::CurrentUser);
        scopes.insert(ApplicationInstallScope::Machine);
        assert_eq!(
            unique_scope(scopes),
            Err(ApplicationUninstallPlatformError::RegistrationChanged)
        );
        assert_eq!(unique_scope(BTreeSet::new()), Ok(None));
    }

    #[test]
    fn elevated_execution_accepts_only_typed_product_codes() {
        assert!(valid_product_code("{9627E855-337D-45EC-A2D9-CBB92B447399}"));
        assert!(!valid_product_code(
            "{9627E855-337D-45EC-A2D9-CBB92B44739&}"
        ));
        assert!(!valid_product_code("9627E855-337D-45EC-A2D9-CBB92B447399"));
    }
}
