//! Fail-closed, handle-bound pipe owner authentication, independent of native queries for controls.
use std::{fmt, io, path::Path};

pub(super) trait Query {
    type Owner;
    type User;
    fn file_is_pipe(&self) -> io::Result<bool>;
    fn pipe_is_client_end(&self) -> io::Result<bool>;
    fn pipe_owner(&self) -> io::Result<Self::Owner>;
    fn current_user(&self) -> io::Result<Self::User>;
    fn owner_is_user(&self, owner: &Self::Owner, user: &Self::User) -> io::Result<bool>;
}

fn verify(query: &impl Query) -> io::Result<()> {
    if !query.file_is_pipe()? {
        return Err(io::Error::other("endpoint is not a pipe"));
    }
    if !query.pipe_is_client_end()? {
        return Err(io::Error::other(
            "handle is not the client end of a named pipe",
        ));
    }
    let owner = query.pipe_owner()?;
    let user = query.current_user()?;
    if !query.owner_is_user(&owner, &user)? {
        return Err(io::Error::other("pipe owner is not the current user"));
    }
    Ok(())
}

/// Use the user SID explicitly for owner as well as DACL: an elevated token's default owner may be a group.
pub(super) fn server_security(user_sid: &str) -> String {
    format!("O:{user_sid}D:P(A;;GA;;;{user_sid})")
}

#[derive(Debug)]
struct Refused {
    endpoint: std::path::PathBuf,
    reason: io::Error,
}
impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unavailable: refusing endpoint {}: server verification failed: {}",
            self.endpoint.display(),
            self.reason
        )
    }
}
impl std::error::Error for Refused {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// The opened stream cannot escape to a protocol writer until every query succeeds.
pub(super) fn checked<S>(stream: S, endpoint: &Path, query: &impl Query) -> io::Result<S> {
    verify(query).map_err(|reason| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            Refused {
                endpoint: endpoint.into(),
                reason,
            },
        )
    })?;
    Ok(stream)
}
pub(super) fn is_refusal(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|error| error.is::<Refused>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    struct Fake {
        pipe: bool,
        client: bool,
        owner: &'static str,
        fail: Option<&'static str>,
        calls: RefCell<Vec<&'static str>>,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                pipe: true,
                client: true,
                owner: "our-user",
                fail: None,
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    impl Fake {
        fn query<T>(&self, stage: &'static str, value: T) -> io::Result<T> {
            self.calls.borrow_mut().push(stage);
            if self.fail == Some(stage) {
                Err(io::Error::other(format!("{stage} failed")))
            } else {
                Ok(value)
            }
        }
    }
    impl Query for Fake {
        type Owner = &'static str;
        type User = &'static str;
        fn file_is_pipe(&self) -> io::Result<bool> {
            self.query("GetFileType", self.pipe)
        }
        fn pipe_is_client_end(&self) -> io::Result<bool> {
            self.query("GetNamedPipeInfo", self.client)
        }
        fn pipe_owner(&self) -> io::Result<Self::Owner> {
            self.query("GetSecurityInfo(owner)", self.owner)
        }
        fn current_user(&self) -> io::Result<Self::User> {
            self.query("OpenProcessToken(current)", ())?;
            self.query("GetTokenInformation(size)", ())?;
            self.query("GetTokenInformation(user)", "our-user")
        }
        fn owner_is_user(&self, owner: &Self::Owner, user: &Self::User) -> io::Result<bool> {
            self.query("EqualSid", owner == user)
        }
    }
    struct Stream(Rc<Cell<usize>>);
    impl io::Write for Stream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.set(self.0.get() + bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn try_hello(query: &Fake) -> (usize, io::Result<()>) {
        use io::Write;
        let writes = Rc::new(Cell::new(0));
        let result = checked(Stream(writes.clone()), Path::new("opaque-endpoint"), query)
            .and_then(|mut stream| stream.write_all(b"hello with credentials\n"));
        (writes.get(), result)
    }

    #[test]
    fn user_owned_pipe_is_checked_before_first_write() {
        let query = Fake::default();
        let (written, result) = try_hello(&query);
        result.unwrap();
        assert!(written > 0);
        assert_eq!(
            *query.calls.borrow(),
            [
                "GetFileType",
                "GetNamedPipeInfo",
                "GetSecurityInfo(owner)",
                "OpenProcessToken(current)",
                "GetTokenInformation(size)",
                "GetTokenInformation(user)",
                "EqualSid"
            ]
        );
    }
    #[test]
    fn foreign_user_group_owner_non_pipe_and_wrong_end_write_nothing() {
        for query in [
            Fake {
                owner: "other-user",
                ..Fake::default()
            },
            Fake {
                owner: "S-1-5-32-544",
                ..Fake::default()
            }, // Administrators, never a substitute for TokenUser.
            Fake {
                pipe: false,
                ..Fake::default()
            },
            Fake {
                client: false,
                ..Fake::default()
            },
        ] {
            let (written, result) = try_hello(&query);
            assert_eq!(written, 0);
            let error = result.unwrap_err();
            assert!(is_refusal(&error));
            assert!(error.to_string().contains("opaque-endpoint"));
            assert!(error.to_string().contains("unavailable"));
        }
    }
    #[test]
    fn every_owner_or_user_query_failure_refuses_before_writing() {
        for stage in [
            "GetFileType",
            "GetNamedPipeInfo",
            "GetSecurityInfo(owner)",
            "OpenProcessToken(current)",
            "GetTokenInformation(size)",
            "GetTokenInformation(user)",
            "EqualSid",
        ] {
            let query = Fake {
                fail: Some(stage),
                ..Fake::default()
            };
            let (written, result) = try_hello(&query);
            assert_eq!(written, 0, "{stage}");
            assert!(is_refusal(&result.unwrap_err()), "{stage}");
            assert_eq!(query.calls.borrow().last(), Some(&stage));
        }
    }
    #[test]
    fn server_security_sets_user_owner_even_when_default_owner_would_be_a_group() {
        let user = "our-user-SID";
        let sddl = server_security(user);
        assert_eq!(sddl, "O:our-user-SIDD:P(A;;GA;;;our-user-SID)");
        assert!(!sddl.contains("S-1-5-32-544"));
    }
    #[test]
    fn refused_probe_does_not_mean_running_or_allow_background_start() {
        let error = checked(
            (),
            Path::new("opaque-endpoint"),
            &Fake {
                owner: "other-user",
                ..Fake::default()
            },
        )
        .unwrap_err();
        assert!(crate::client::core_probe_result::<()>(Err(error)).is_err());
        assert!(crate::client::core_probe_result(Ok(())).unwrap());
        assert!(
            !crate::client::core_probe_result::<()>(Err(io::ErrorKind::NotFound.into())).unwrap()
        );
    }
}
