# parrot-link

Links between Parrot runtimes (spec 24 and Appendix E9). It holds the carriers, the Noise sessions,
the framing and pairing. The Noise session ends in the runtime; `parrot connect --link` only relays bytes to
the link endpoint.

| Module | What it does |
| --- | --- |
| `keys` | The static X25519 key in `link.key` (mode `0600`), runtime ids, pinned keys |
| `noise` | `Dialer` and `Listener`: the handshake and the encrypted connection |
| `frame` | Length prefixes, message parts inside Noise messages, `blob/put` chunks |
| `pairing` | Invite codes, pending invites, the simultaneous-dial rule |
| `carrier` | `ssh`, `command`, `tcp` and carrier eggs, with their containment |
| `endpoint` | `link.sock` beside `sock`, with the same directory rules |

## Handshake

The dialler sends a plaintext hello with the mode in it, then runs
`Noise_XX_25519_ChaChaPoly_BLAKE2s`. Pairing with a code runs `Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s`
with the code's secret. The hello is part of the Noise prologue, so a carrier can't change it unseen.
When the dialler's carrier takes messages under 64 KiB, the hello says so as `maxMessage`, and the
listener doesn't send a Noise message larger than that either.

- The dialler checks the responder's key before it sends its own.
- Outside pairing, the listener accepts only pinned keys.
- A pairing code works once and expires after 10 minutes by default.

## Containment

| Carrier | Containment |
| --- | --- |
| `ssh` | `shell`, unless the peer restricts the key to `parrot connect --link` |
| `command` | `shell` |
| `tcp` | `restricted` |
| Carrier egg | As declared, else `unknown`, which counts as `shell` |

`parrot link invite --authorized-key "<public key>"` writes the peer's key to `~/.ssh/authorized_keys`
with `command="parrot connect --link",restrict`, and removes other lines for the same key. The peer
pairs with the code over `ssh`, and its link shows `restricted` instead of `shell`.
