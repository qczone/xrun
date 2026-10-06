use super::links::{endpoint, parse_link};
use super::*;
use crate::membership::Manager;

#[test]
fn public_relay_preserves_network_identity_through_pairing_and_revocation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let address = format!("https://relay.example/{}", crate::crypto::random_token());
    let (manager, _, _, _) = Manager::create(
        &dir.path().join("manager"),
        "manager1",
        vec![address.clone()],
        String::new(),
    )?;
    let roster = manager.roster()?;
    let link = invitation_link(&roster, &crate::crypto::random_token())?;
    let invitation = parse_link(&link)?;
    assert_eq!(invitation.addresses, vec![address.clone()]);
    assert_eq!(invitation.relay_pin, "webpki");
    assert_eq!(invitation.root_pin, crypto::ca_spki_pin(&roster.ca_pem)?);
    let endpoint = endpoint(&address)?;
    assert_eq!(endpoint.addresses, vec![address]);
    assert_eq!(endpoint.pin, "webpki");
    // The manager's signature binds the transport trust policy too.
    let mut tampered = roster.clone();
    tampered.roster.relay_ca_pem = roster.ca_pem.clone();
    assert!(tampered.verify(&roster.roster.network_id).is_err());
    let cache = RosterCache::open(&dir.path().join("roster.db"))?;
    cache.observe(&roster.roster.network_id, &roster)?;
    let (_, csr) = crypto::new_device_request()?;
    let paired = manager.pair(&manager.invite(false)?, "member1", &csr)?;
    cache.observe(&roster.roster.network_id, &paired.roster)?;
    drop(manager);
    let manager = Manager::open(&dir.path().join("manager"))?;
    let revoked = manager.revoke("member1")?;
    cache.observe(&roster.roster.network_id, &revoked)?;
    assert!(
        cache
            .load(&roster.roster.network_id)?
            .member("member1")?
            .revoked
    );
    assert!(manager.pair("", "member1", &csr).is_err());
    Ok(())
}

#[test]
fn public_relay_requires_an_unambiguous_secret_route() {
    for value in [
        "https://relay.example",
        "https://relay.example/wrong",
        "http://relay.example/abcdefghijklmnopqrstuvwxyz",
        "https://user@relay.example/abcdefghijklmnopqrstuvwxyz",
        "https://relay.example/abcdefghijklmnopqrstuvwxyz/",
        "https://relay.example/abcdefghijklmnopqrstuvwxyz?x=1",
        "https://relay.example/abcdefghijklmnopqrstuvwxyz#x",
    ] {
        assert!(endpoint(value).is_err(), "accepted {value}");
    }
}
