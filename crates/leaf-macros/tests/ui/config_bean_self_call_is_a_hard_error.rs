//! An intra-config `#[bean]`→`#[bean]` self-call (`self.other_bean()` inside a
//! `#[bean]` body) is a loud `compile_error!` with a rewrite hint: under leaf's
//! lite-only `#[configuration]` model the self-call returns a SECOND unmanaged
//! instance (not the managed singleton). The remediation is to take the collaborator
//! as a `Ref<T>` parameter so the container injects the managed bean (phase3/05).

use leaf_macros::configuration;

struct Repo;
struct Service;

#[configuration]
impl AppConfig {
    #[bean]
    fn repo(&self) -> Repo {
        Repo
    }

    #[bean]
    fn service(&self) -> Service {
        // The footgun: calling the sibling #[bean] `repo` directly returns a fresh
        // unmanaged Repo, NOT the managed singleton.
        let _r = self.repo();
        Service
    }
}

struct AppConfig;

fn main() {}
