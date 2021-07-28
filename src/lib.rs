use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use rfc7239::{parse, Forwarded, NodeIdentifier, NodeName};
use std::convert::Infallible;
use std::iter::{once, FromIterator, IntoIterator};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use warp::filters::addr::remote;
use warp::Filter;

/// Represents a set of IP networks.
#[derive(Debug, Clone)]
pub struct IpNetworks {
    networks: Vec<IpNetwork>,
}

impl IpNetworks {
    /// Checks if addr is part of any IP networks included.
    pub fn contains(&self, addr: &IpAddr) -> bool {
        self.networks.iter().any(|&network| network.contains(*addr))
    }

    /// Special constructor that builds IpNetwork from an iterator of IP addresses.
    pub fn from_ipaddr_iter<'a, T: Iterator<Item = &'a IpAddr>>(addrs: T) -> Self {
        Self::from_iter(addrs.map(|&addr| -> IpNetwork {
            match addr {
                IpAddr::V4(addr) => Ipv4Network::from(addr).into(),
                IpAddr::V6(addr) => Ipv6Network::from(addr).into(),
            }
        }))
    }
}

impl From<Vec<IpAddr>> for IpNetworks {
    fn from(addrs: Vec<IpAddr>) -> Self {
        Self::from_ipaddr_iter(addrs.iter())
    }
}

impl From<&Vec<IpAddr>> for IpNetworks {
    fn from(addrs: &Vec<IpAddr>) -> Self {
        Self::from_ipaddr_iter(addrs.iter())
    }
}

impl FromIterator<IpNetwork> for IpNetworks {
    fn from_iter<T: IntoIterator<Item = IpNetwork>>(addrs: T) -> Self {
        IpNetworks {
            networks: Vec::<IpNetwork>::from_iter(addrs),
        }
    }
}

/// Creates a `Filter` that provides the "real ip" of the connected client.
///
/// This uses the "x-forwarded-for" or "x-real-ip" headers set by reverse proxies.
/// To stop clients from abusing these headers, only headers set by trusted remotes will be accepted.
///
/// Note that if multiple forwarded-for addresses are present, wich can be the case when using nested reverse proxies,
/// all proxies in the chain have to be within the list of trusted proxies.
///
/// ## Example
///
/// ```no_run
/// use warp::Filter;
/// use warp_real_ip::real_ip;
/// use std::net::IpAddr;
///
/// let proxy_addr = [127, 10, 0, 1].into();
/// warp::any()
///     .and(real_ip(vec![proxy_addr]))
///     .map(|addr: Option<IpAddr>| format!("Hello {}", addr.unwrap()));
/// ```
pub fn real_ip(
    trusted_proxies: impl Into<IpNetworks>,
) -> impl Filter<Extract = (Option<IpAddr>,), Error = Infallible> + Clone {
    let trusted_proxies = trusted_proxies.into();
    remote().and(get_forwarded_for()).map(
        move |addr: Option<SocketAddr>, forwarded_for: Vec<IpAddr>| {
            addr.map(|addr| {
                let hops = forwarded_for.iter().copied().chain(once(addr.ip()));
                for hop in hops.rev() {
                    if !trusted_proxies.contains(&hop) {
                        return hop;
                    }
                }

                // all hops were trusted, return the last one
                forwarded_for.first().copied().unwrap_or(addr.ip())
            })
        },
    )
}

/// Creates a `Filter` that extracts the ip addresses from the the "forwarded for" chain
pub fn get_forwarded_for() -> impl Filter<Extract = (Vec<IpAddr>,), Error = Infallible> + Clone {
    warp::header("x-forwarded-for")
        .map(|list: CommaSeparated<IpAddr>| list.into_inner())
        .or(warp::header("x-real-ip").map(
            |ip: String| IpAddr::from_str(maybe_bracketed(&maybe_quoted(ip)))
                .map_or_else(|_| Vec::<IpAddr>::new(), |x| vec![x])
        ))
        .unify()
        .or(warp::header("forwarded").map(|header: String| {
            parse(&header)
                .filter_map(|forward| match forward {
                    Ok(Forwarded {
                        forwarded_for:
                            Some(NodeIdentifier {
                                name: NodeName::Ip(ip),
                                ..
                            }),
                        ..
                    }) => Some(ip),
                    _ => None,
                })
                .collect::<Vec<_>>()
        }))
        .unify()
        .or(warp::any().map(|| vec![]))
        .unify()
}

enum CommaSeparatedIteratorState {
    Default,
    Quoted,
    QuotedPair,
    Token,
    PostambleForQuoted,
}

struct CommaSeparatedIterator<'a> {
    /// target
    target: &'a str,
    /// iterator
    char_indices: std::str::CharIndices<'a>,
    /// current scanner state
    state: CommaSeparatedIteratorState,
    /// start position of the last token found
    s: usize,
}

impl<'a> CommaSeparatedIterator<'a> {
    pub fn new(target: &'a str) -> Self {
        Self {
            target: target,
            char_indices: target.char_indices(),
            state: CommaSeparatedIteratorState::Default,
            s: 0,
        }
    }
}

impl<'a> Iterator for CommaSeparatedIterator<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.char_indices.next() {
                Some((i, c)) => match match self.state {
                    CommaSeparatedIteratorState::Default => match c {
                        '"' => {
                            self.s = i;
                            (None, CommaSeparatedIteratorState::Quoted)
                        }
                        ' ' | '\t' => (None, CommaSeparatedIteratorState::Default),
                        ',' => (
                            Some(Some(&self.target[i..i])),
                            CommaSeparatedIteratorState::Default,
                        ),
                        _ => {
                            self.s = i;
                            (None, CommaSeparatedIteratorState::Token)
                        }
                    },
                    CommaSeparatedIteratorState::Quoted => match c {
                        '"' => (
                            Some(Some(&self.target[self.s..i + 1])),
                            CommaSeparatedIteratorState::PostambleForQuoted,
                        ),
                        '\\' => (None, CommaSeparatedIteratorState::QuotedPair),
                        _ => (None, CommaSeparatedIteratorState::Quoted),
                    },
                    CommaSeparatedIteratorState::QuotedPair => {
                        (None, CommaSeparatedIteratorState::Quoted)
                    }
                    CommaSeparatedIteratorState::Token => match c {
                        ',' => (
                            Some(Some(&self.target[self.s..i])),
                            CommaSeparatedIteratorState::Default,
                        ),
                        _ => (None, CommaSeparatedIteratorState::Token),
                    },
                    CommaSeparatedIteratorState::PostambleForQuoted => match c {
                        ',' => (None, CommaSeparatedIteratorState::Default),
                        _ => (None, CommaSeparatedIteratorState::PostambleForQuoted),
                    },
                } {
                    (Some(next), next_state) => {
                        self.state = next_state;
                        return next;
                    }
                    (None, next_state) => {
                        self.state = next_state;
                    }
                },
                None => break,
            }
        }
        return match self.state {
            CommaSeparatedIteratorState::Default
            | CommaSeparatedIteratorState::PostambleForQuoted => None,
            CommaSeparatedIteratorState::Quoted | CommaSeparatedIteratorState::QuotedPair => {
                self.state = CommaSeparatedIteratorState::Default;
                Some(&self.target[self.s..])
            }
            CommaSeparatedIteratorState::Token => {
                self.state = CommaSeparatedIteratorState::Default;
                Some(&self.target[self.s..])
            }
        };
    }
}

pub fn maybe_quoted<T: AsRef<str>>(x: T) -> String {
    let x = x.as_ref();
    let mut i = x.chars();
    if i.next() == Some('"') {
        let mut s = String::with_capacity(x.len());
        let mut state = 0;
        for c in i {
            state = match state {
                0 => match c {
                    '"' => break,
                    '\\' => 1,
                    _ => {
                        s.push(c);
                        0
                    }
                },
                _ => {
                    s.push(c);
                    0
                }
            };
        }
        s
    } else {
        x.to_string()
    }
}

pub fn maybe_bracketed<'a>(x: &'a str) -> &'a str {
    if x.as_bytes()[0] == ('[' as u8) && x.as_bytes()[x.len() - 1] == (']' as u8) {
        &x[1..x.len() - 1]
    } else {
        x
    }
}

/// Newtype so we can implement FromStr
struct CommaSeparated<T>(Vec<T>);

impl<T> CommaSeparated<T> {
    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

impl<T: FromStr> FromStr for CommaSeparated<T> {
    type Err = T::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let vec = CommaSeparatedIterator::new(s)
            .map(|x| T::from_str(maybe_bracketed(&maybe_quoted(x.trim()))))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CommaSeparated(vec))
    }
}

#[cfg(test)]
mod tests {
    use crate::{CommaSeparatedIterator, maybe_quoted, maybe_bracketed};

    #[test]
    fn test_comma_separated_iterator() {
        assert_eq!(vec!["abc", "def", "ghi", "jkl ", "mno", "pqr"], CommaSeparatedIterator::new("abc,def, ghi,\tjkl , mno,\tpqr").collect::<Vec<&str>>());
        assert_eq!(vec!["abc", "\"def\"", "\"ghi\"", "\"jkl\"", "\"mno\"", "pqr"], CommaSeparatedIterator::new("abc,\"def\", \"ghi\",\t\"jkl\" , \"mno\",\tpqr").collect::<Vec<&str>>());
    }

    #[test]
    fn test_maybe_quoted() {
        assert_eq!("abc", maybe_quoted("abc"));
        assert_eq!("abc", maybe_quoted("\"abc\""));
        assert_eq!("a\"bc", maybe_quoted("\"a\\\"bc\""));
    }

    #[test]
    fn test_maybe_bracketed() {
        assert_eq!("abc", maybe_bracketed("abc"));
        assert_eq!("abc", maybe_bracketed("[abc]"));
        assert_eq!("[abc", maybe_bracketed("[abc"));
        assert_eq!("abc]", maybe_bracketed("abc]"));
    }

}
