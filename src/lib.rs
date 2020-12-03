use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use warp::filters::addr::remote;
use warp::Filter;

/// Creates a `Filter` that provides the "real ip" of the connected client.
///
/// This uses the "x-forwarded-for" or "x-real-ip" headers set by reverse proxies.
/// To stop clients from abusing these headers, only headers set by trusted remotes will be accepted.
pub fn real_ip(
    trusted_proxies: Vec<IpAddr>,
) -> impl Filter<Extract = (Option<IpAddr>,), Error = Infallible> + Clone {
    let forwarded_for = warp::header::<IpAddr>("x-forwarded-for")
        .or(warp::header("x-real-ip"))
        .unify()
        .map(Some)
        .or(warp::any().map(|| None))
        .unify();

    remote().and(forwarded_for).map(
        move |addr: Option<SocketAddr>, forwarded_for: Option<IpAddr>| {
            addr.map(|addr| {
                let ip = addr.ip();
                if trusted_proxies.contains(&ip) {
                    forwarded_for.unwrap_or(ip)
                } else {
                    ip
                }
            })
        },
    )
}
