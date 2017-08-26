//! etcd's authentication and authorization API.
//!
//! These API endpoints are used to manage users and roles.

use std::str::FromStr;

use futures::{Future, IntoFuture, Stream};
use hyper::{StatusCode, Uri};
use hyper::client::Connect;
use serde_json;

use async::first_ok;
use client::{Client, ClusterInfo, Response};
use error::{ApiError, Error};

/// The structure returned by the `GET /v2/auth/enable` endpoint.
#[derive(Debug, Deserialize)]
struct AuthStatus {
    /// Whether or not the auth system is enabled.
    pub enabled: bool,
}

/// The result of attempting to disable the auth system.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DisableAuth {
    /// The auth system was already disabled.
    AlreadyDisabled,
    /// The auth system was successfully disabled.
    Disabled,
    /// The attempt to disable the auth system was not done by a root user.
    Unauthorized,
}

impl DisableAuth {
    /// Indicates whether or not the auth system was disabled as a result of the call to `disable`.
    pub fn is_disabled(&self) -> bool {
        match *self {
            DisableAuth::AlreadyDisabled | DisableAuth::Disabled => true,
            DisableAuth::Unauthorized => false,
        }
    }
}

/// The result of attempting to enable the auth system.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EnableAuth {
    /// The auth system was already enabled.
    AlreadyEnabled,
    /// The auth system was successfully enabled.
    Enabled,
    /// The auth system could not be enabled because there is no root user.
    RootUserRequired,
}

impl EnableAuth {
    /// Indicates whether or not the auth system was enabled as a result of the call to `enable`.
    pub fn is_enabled(&self) -> bool {
        match *self {
            EnableAuth::AlreadyEnabled | EnableAuth::Enabled => true,
            EnableAuth::RootUserRequired => false,
        }
    }
}

/// An existing etcd user.
#[derive(Debug, Clone, Deserialize, Eq, Hash, PartialEq)]
pub struct User {
    /// The user's name.
    name: String,
    /// Roles granted to the user.
    roles: Vec<Role>,
}

impl User {
    /// Returns the user's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the roles granted to the user.
    pub fn roles(&self) -> &[Role] {
        &self.roles
    }
}

/// Paramters used to create a new etcd user.
#[derive(Debug, Clone, Eq, Hash, PartialEq, Serialize)]
pub struct NewUser {
    /// The user's name.
    name: String,
    /// The user's password.
    password: String,
    /// An initial set of roles granted to the user.
    roles: Option<Vec<String>>,
}

impl NewUser {
    /// Creates a new user.
    pub fn new<N, P>(name: N, password: P) -> Self
    where
        N: Into<String>,
        P: Into<String>,
    {
        NewUser {
            name: name.into(),
            password: password.into(),
            roles: None,
        }
    }

    /// Grants a role to the new user.
    pub fn add_role<R>(&mut self, role: R)
    where
        R: Into<String>,
    {
        match self.roles {
            Some(ref mut roles) => roles.push(role.into()),
            None => self.roles = Some(vec![role.into()]),
        }
    }
}

/// Parameters used to update an existing etcd user.
#[derive(Debug, Clone, Eq, Hash, PartialEq, Serialize)]
pub struct UserUpdate {
    /// The user's name.
    name: String,
    /// A new password for the user.
    password: Option<String>,
    /// Roles being granted to the user.
    #[serde(rename = "grant")]
    grants: Option<Vec<String>>,
    /// Roles being revoked from the user.
    #[serde(rename = "revoke")]
    revocations: Option<Vec<String>>,
}

impl UserUpdate {
    /// Creates a new `UserUpdate` for the given user.
    pub fn new<N>(name: N) -> Self
    where
        N: Into<String>,
    {
        UserUpdate {
            name: name.into(),
            password: None,
            grants: None,
            revocations: None,
        }
    }

    /// Updates the user's password.
    pub fn update_password<P>(&mut self, password: P)
    where
        P: Into<String>,
    {
        self.password = Some(password.into());
    }

    /// Grants the given role to the user.
    pub fn grant_role<R>(&mut self, role: R)
    where
        R: Into<String>,
    {
        match self.grants {
            Some(ref mut grants) => grants.push(role.into()),
            None => self.grants = Some(vec![role.into()]),
        }
    }

    /// Revokes the given role from the user.
    pub fn revoke_role<R>(&mut self, role: R)
    where
        R: Into<String>,
    {
        match self.revocations {
            Some(ref mut revocations) => revocations.push(role.into()),
            None => self.revocations = Some(vec![role.into()]),
        }
    }
}

/// An authorization role.
#[derive(Debug, Deserialize, Clone, Eq, Hash, PartialEq, Serialize)]
pub struct Role {
    /// The name of the role.
    name: String,
    /// Permissions granted to the role.
    permissions: Permissions,
}

impl Role {
    /// Creates a new role.
    pub fn new<N>(name: N) -> Self
    where
        N: Into<String>,
    {
        Role {
            name: name.into(),
            permissions: Permissions::new(),
        }
    }

    /// Grants read permission for a key in etcd's key-value store to this role.
    pub fn add_kv_read_permission<K>(&mut self, key: K)
    where
        K: Into<String>,
    {
        self.permissions.kv.add_read_permission(key)
    }

    /// Grants write permission for a key in etcd's key-value store to this role.
    pub fn add_kv_write_permission<K>(&mut self, key: K)
    where
        K: Into<String>,
    {
        self.permissions.kv.add_write_permission(key)
    }

    /// Returns a list of keys in etcd's key-value store that this role is allowed to read.
    pub fn kv_read_permissions(&self) -> &[String] {
        &self.permissions.kv.read
    }

    /// Returns a list of keys in etcd's key-value store that this role is allowed to write.
    pub fn kv_write_permissions(&self) -> &[String] {
        &self.permissions.kv.write
    }
}

/// Parameters used to update an existing authorization role.
#[derive(Debug, Clone, Eq, Hash, PartialEq, Serialize)]
pub struct RoleUpdate {
    /// The name of the role.
    name: String,
    /// Permissions being added to the role.
    #[serde(rename = "grant")]
    grants: Permissions,
    /// Permissions being removed from the role.
    #[serde(rename = "revoke")]
    revocations: Permissions,
}

impl RoleUpdate {
    /// Creates a new `RoleUpdate` for the given role.
    pub fn new<R>(role: R) -> Self
    where
        R: Into<String>,
    {
        RoleUpdate {
            name: role.into(),
            grants: Permissions::new(),
            revocations: Permissions::new(),
        }
    }

    /// Grants read permission for a key in etcd's key-value store to this role.
    pub fn grant_kv_read_permission<K>(&mut self, key: K)
    where
        K: Into<String>,
    {
        self.grants.kv.add_read_permission(key)
    }

    /// Grants write permission for a key in etcd's key-value store to this role.
    pub fn grant_kv_write_permission<K>(&mut self, key: K)
    where
        K: Into<String>,
    {
        self.grants.kv.add_write_permission(key)
    }

    /// Revokes read permission for a key in etcd's key-value store from this role.
    pub fn revoke_kv_read_permission<K>(&mut self, key: &K)
    where
        K: Into<String>,
        String: PartialEq<K>,
    {
        self.revocations.kv.remove_read_permission(key)
    }

    /// Revokes write permission for a key in etcd's key-value store from this role.
    pub fn revoke_kv_write_permission<K>(&mut self, key: &K)
    where
        K: Into<String>,
        String: PartialEq<K>,
    {
        self.revocations.kv.remove_write_permission(key)
    }
}

/// The access permissions granted to a role.
#[derive(Debug, Deserialize, Clone, Eq, Hash, PartialEq, Serialize)]
struct Permissions {
    /// Permissions for etcd's key-value store.
    kv: Permission,
}

impl Permissions {
    /// Creates a new set of permissions.
    fn new() -> Self {
        Permissions {
            kv: Permission::new(),
        }
    }
}

/// A set of read and write access permissions for etcd resources.
#[derive(Debug, Deserialize, Clone, Eq, Hash, PartialEq, Serialize)]
struct Permission {
    /// Resources allowed to be read.
    read: Vec<String>,
    /// Resources allowed to be written.
    write: Vec<String>,
}

impl Permission {
    /// Creates a new permission record.
    fn new() -> Self {
        Permission {
            read: Vec::new(),
            write: Vec::new(),
        }
    }

    /// Grants read access to a resource.
    fn add_read_permission<K>(&mut self, key: K)
    where
        K: Into<String>,
    {
        self.read.push(key.into())
    }

    /// Grants write access to a resource.
    fn add_write_permission<K>(&mut self, key: K)
    where
        K: Into<String>,
    {
        self.write.push(key.into())
    }

    /// Revokes read access to a resource.
    fn remove_read_permission<K>(&mut self, key: &K)
    where
        K: Into<String>,
        String: PartialEq<K>,
    {
        if let Some(position) = self.read.iter().position(|k| k == key) {
            self.read.remove(position);
        }
    }

    /// Revokes write access to a resource.
    fn remove_write_permission<K>(&mut self, key: &K)
    where
        K: Into<String>,
        String: PartialEq<K>,
    {
        if let Some(position) = self.write.iter().position(|k| k == key) {
            self.write.remove(position);
        }
    }
}

/// Attempts to disable the auth system.
pub fn disable<C>(
    client: &Client<C>,
) -> Box<Future<Item = Response<DisableAuth>, Error = Vec<Error>>>
where
    C: Clone + Connect,
{
    let http_client = client.http_client().clone();

    let result = first_ok(client.endpoints().to_vec(), move |member| {
        let url = build_url(member, "/enable");
        let uri = Uri::from_str(url.as_str())
            .map_err(Error::from)
            .into_future();

        let http_client = http_client.clone();

        let response = uri.and_then(move |uri| http_client.delete(uri).map_err(Error::from));

        let result = response.and_then(|response| {
            let status = response.status();
            let cluster_info = ClusterInfo::from(response.headers());

            let result = match status {
                StatusCode::Ok => Response {
                    data: DisableAuth::Disabled,
                    cluster_info,
                },
                StatusCode::Conflict => Response {
                    data: DisableAuth::AlreadyDisabled,
                    cluster_info,
                },
                StatusCode::Unauthorized => Response {
                    data: DisableAuth::Unauthorized,
                    cluster_info,
                },
                _ => return Err(Error::UnexpectedStatus(status)),
            };

            Ok(result)
        });

        Box::new(result)
    });

    Box::new(result)
}

/// Attempts to enable the auth system.
pub fn enable<C>(client: &Client<C>) -> Box<Future<Item = Response<EnableAuth>, Error = Vec<Error>>>
where
    C: Clone + Connect,
{
    let http_client = client.http_client().clone();

    let result = first_ok(client.endpoints().to_vec(), move |member| {
        let url = build_url(member, "/enable");
        let uri = Uri::from_str(url.as_str())
            .map_err(Error::from)
            .into_future();

        let http_client = http_client.clone();

        let response = uri.and_then(move |uri| {
            http_client.put(uri, "".to_owned()).map_err(Error::from)
        });

        let result = response.and_then(|response| {
            let status = response.status();
            let cluster_info = ClusterInfo::from(response.headers());

            let result = match status {
                StatusCode::Ok => Response {
                    data: EnableAuth::Enabled,
                    cluster_info,
                },
                StatusCode::BadRequest => Response {
                    data: EnableAuth::RootUserRequired,
                    cluster_info,
                },
                StatusCode::Conflict => Response {
                    data: EnableAuth::AlreadyEnabled,
                    cluster_info,
                },
                _ => return Err(Error::UnexpectedStatus(status)),
            };

            Ok(result)
        });

        Box::new(result)
    });

    Box::new(result)
}

/// Determines whether or not the auth system is enabled.
pub fn status<C>(client: &Client<C>) -> Box<Future<Item = Response<bool>, Error = Vec<Error>>>
where
    C: Clone + Connect,
{
    let http_client = client.http_client().clone();

    let result = first_ok(client.endpoints().to_vec(), move |member| {
        let url = build_url(member, "/enable");
        let uri = Uri::from_str(url.as_str())
            .map_err(Error::from)
            .into_future();

        let http_client = http_client.clone();

        let response = uri.and_then(move |uri| http_client.get(uri).map_err(Error::from));

        let result = response.and_then(|response| {
            let status = response.status();
            let cluster_info = ClusterInfo::from(response.headers());
            let body = response.body().concat2().map_err(Error::from);

            body.and_then(move |ref body| if status == StatusCode::Ok {
                match serde_json::from_slice::<AuthStatus>(body) {
                    Ok(data) => Ok(Response {
                        data: data.enabled,
                        cluster_info,
                    }),
                    Err(error) => Err(Error::Serialization(error)),
                }
            } else {
                match serde_json::from_slice::<ApiError>(body) {
                    Ok(error) => Err(Error::Api(error)),
                    Err(error) => Err(Error::Serialization(error)),
                }
            })
        });

        Box::new(result)
    });

    Box::new(result)
}

/// Constructs the full URL for an API call.
fn build_url(endpoint: &Uri, path: &str) -> String {
    let maybe_slash = if endpoint.as_ref().ends_with("/") {
        ""
    } else {
        "/"
    };

    format!("{}{}v2/auth{}", endpoint, maybe_slash, path)
}
