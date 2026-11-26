# Protocol Reference (SDK view)

This is a developer reference for how `uniden-protocol` and `uniden-sds`
model the protocol. For sources and verification status, see
[protocol-compliance.md](protocol-compliance.md).

## Wire format

```text
request   = NAME *("," ARG) CR            ; NAME = 2-8 uppercase letters
line-rep  = NAME *("," FIELD) CR
xml-rep   = NAME ",<XML>," [CR [LF]] xml-document
error     = "ERR" | "NG" | "FER" | "ORER" | NAME ",ERR" ["," CODE] | NAME ",NG"
footer    = "<Footer" | "<Foot"  ' No="' n '" EOT="' ("0"|"1") '"/>'
```

- Command arguments carry frequencies as integer counts of 100 Hz units.
  XML attributes carry them as `" 154.4150MHz"`.
- Indexes (`System@Index`, ...) used by `HLD`/`AVD` are valid only while
  `MonitorList@DB_Counter` is unchanged.

## Crate map

| Need | API |
|---|---|
| Encode a command | `Command::encode()` |
| Is it safe to retry? | `Command::effect()` |
| Where does knowledge come from? | `Command::provenance()` |
| Reassemble datagrams | `FrameAssembler::push()` |
| Decode a reply | `decode_reply(&command, frame, &XmlLimits)` |
| High-level async API | `uniden_sds::Scanner` |
| Push updates | `Scanner::start_push(ms)` + `Scanner::subscribe()` |
| Feature detection | `Scanner::probe_capabilities()` (read-only probes) |

## SDK error semantics

| Error | Meaning | Retried? |
|---|---|---|
| `Timeout { state_change_unknown: false }` | Read-only command; all attempts timed out | already retried `retries` times |
| `Timeout { state_change_unknown: true }` | Write command; **may have executed** | never |
| `Rejected { error: CommandError }` | `ERR`: invalid or unsupported | never |
| `Rejected { error: NotAvailable }` | `NG`: not possible in the current state | never |
| `Decode` | Replies arrived but none decoded | read-only: yes |
| `QueueFull` / `Closed` | Back-pressure / shut down | n/a |
