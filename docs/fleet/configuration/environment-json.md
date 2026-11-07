# Environment JSON file and command output

An [EnvProvider](../resources/env-provider.md) file source reads a UTF-8 JSON
object whose values are strings. Command sources must emit this same format
on stdout and exit zero. This is environment data, separate from Fleet's
[native TOML configuration](fleet-toml.md) and JSON resource manifests.

Save this example as `agent-env.json` in a private manager-local directory:

```json
{
  "PROJECT_MODE": "review",
  "GREETING": "bonjour = 🌍",
  "MULTILINE": "first line\nsecond line"
}
```

| Rule | Accepted input |
| --- | --- |
| Shape | One JSON object of string values; empty `{}` is valid. |
| Names | `[A-Za-z_][A-Za-z0-9_]*`; no case collisions such as `Path` and `PATH`. |
| Values | Strings, including empty strings, Unicode, spaces, `=` and newlines; never NUL. |
| Source size | At most 64 KiB of file/command bytes. |
| Map/merged size | Total UTF-8 key and value bytes at most 64 KiB. |

Arrays, nested objects, numbers, booleans and null values are refused. The
same key/value rules apply to literal EnvProvider `data`, TaskConfig `env`
and Task `env`. Across maps, later values replace case variants and retain
the winning spelling on every platform. See
[the complete precedence](../resources/task-config.md#referencing-and-overriding-defaults).

A file must be a regular file at an absolute path on the manager. There is
no lookup beside the TOML config, resource manifest, Task workspace or remote
host. Use a native path; Windows backslashes must be escaped inside the
provider's JSON manifest. A missing, unreadable, nonregular, malformed or
oversized file prevents launch. Source parse errors never echo its content.

External files may contain credentials: restrict their access and exclude
them from public repositories. Fleet freezes their resolved values in a
private plaintext launch snapshot; protect store backups as well. Editing
the file affects future resolutions, not already frozen Tasks. Follow
[the snapshot recovery rules](../resources/task-config.md#resolution-and-freezing)
for an existing execution; there is no background refresh.
