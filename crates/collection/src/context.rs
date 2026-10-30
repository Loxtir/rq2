//! Everything a request needs from its surroundings: the collection, the
//! selected environment, globals, and the resolved deployment target.
//!
//! The CLI, sending, the cURL preview and query variables all build this the
//! same way, so the scope chain cannot drift between them.

use crate::deployment::{self, Target};
use crate::model::{Auth, Collection, Deployment, Environment, RequestDef, Variable};
use crate::send::{self, Variables};

pub struct RequestContext<'a> {
    pub collection: &'a Collection,
    pub environment: Option<&'a Environment>,
    pub globals: &'a [Variable],
    pub target: Option<Target>,
}

impl<'a> RequestContext<'a> {
    /// Resolves the deployment target up front, so a bad selection fails before
    /// anything is sent or rendered.
    pub fn new(
        collection: &'a Collection,
        environment: Option<&'a Environment>,
        globals: &'a [Variable],
        deployment: Option<&Deployment>,
    ) -> Result<Self, String> {
        let target = deployment::resolve(collection, environment, deployment)?;
        Ok(Self {
            collection,
            environment,
            globals,
            target,
        })
    }

    pub fn variables(&self, path: &[usize]) -> Variables {
        send::variables_for_target(
            self.collection,
            self.environment,
            self.globals,
            path,
            self.target.as_ref(),
        )
    }

    /// The same values with every secret replaced, for anything shown or copied.
    pub fn masked_variables(&self, path: &[usize]) -> Variables {
        crate::curl::masked_variables_for_target(
            self.collection,
            self.environment,
            self.globals,
            path,
            self.target.as_ref(),
        )
    }

    /// The auth that applies to `request` at `path`: its own setting (which an
    /// editor may have just changed) resolved against everything above it.
    pub fn auth<'r>(&'r self, path: &[usize], request: &'r RequestDef) -> Option<&'r Auth> {
        request
            .auth
            .resolve(self.collection.inherited_auth_at(path))
    }

    pub fn insecure(&self) -> bool {
        self.environment
            .is_some_and(|environment| environment.insecure)
    }
}
