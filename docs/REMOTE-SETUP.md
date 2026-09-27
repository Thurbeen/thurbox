# Run a session on another machine

Thurbox stays on your computer. The agent, repository and tmux window run on
the selected host. Start with [a plain Linux SSH host](#plain-linux-ssh); the
other routes only change how OpenSSH reaches that host.

| Your environment | Start here |
| --- | --- |
| Reachable Linux machine | [Plain Linux SSH](#plain-linux-ssh) |
| Private machine behind a jump host | [SSH bastion](#ssh-bastion) |
| AWS EC2 through Systems Manager | [AWS](#aws-systems-manager) |
| Google Compute Engine through IAP | [Google Cloud](#google-cloud-iap) |
| Azure VM through Bastion | [Azure](#azure-bastion) |
| Another local WSL distro | [WSL](#wsl) |

The cloud configurations below were checked against the linked provider docs.
They were **not connected to a cloud account** during this guide's validation.
Supply your own identity, permissions, network rules, and SSH key; Thurbox
does not manage cloud credentials or IAM. The generic path was exercised with
the repository's isolated SSH container test.

## Plain Linux SSH

1. On your computer, install Thurbox and its companion `thurbox-cli`, OpenSSH
   client, and an SSH key accepted by the server. On the Linux host, run
   `sshd` and install Git, tmux **3.2 or newer**, your chosen coding-agent CLI,
   and that agent's own credentials. Clone your repository there. The path you
   pass to Thurbox is the **host's absolute path**, not your local checkout.
   After creating the alias in step 2, check the host's prerequisites and
   the non-interactive command path:

   ```sh
   ssh devbox 'git --version; tmux -V; command -v codex; test -d /srv/project/.git'
   ```

   Replace `codex` with your configured agent and `/srv/project` with your
   host's repository path. Make sure the agent can authenticate on the host
   before creating a session. If its executable is absent from the remote
   command `PATH`, set `path_prepend` in `hosts.toml` or check the host's login
   shell; see [configuration](CONFIG.md#hoststoml).

2. Give OpenSSH a reusable alias in **your local** `~/.ssh/config`:

   ```sshconfig
   Host devbox
       HostName devbox.example.com
       User remote-user
       IdentityFile ~/.ssh/id_ed25519
       IdentitiesOnly yes
   ```

   Replace the example hostname and user. Confirm `ssh devbox 'printf ready'`
   prints `ready` without an interactive password prompt. Verify the host key
   through your normal SSH process; do not disable host-key checking.

3. Add this entry to **your local** `~/.config/thurbox/hosts.toml` (create the
   file if needed):

   ```toml
   [[hosts]]
   name = "devbox"
   destination = "devbox"
   ```

   Restart Thurbox after editing `hosts.toml`. The `name` is what `--host`
   takes; `destination` is the OpenSSH alias. With the default
   `share_sessions = true`, the host's Thurbox database records its sessions
   and other Thurbox instances using that host can see them. On first use,
   Thurbox provisions its CLI on a compatible host when missing. Set
   `share_sessions = false` only when you want this local instance to own the
   records instead; see [the full field reference](CONFIG.md#hoststoml).

4. Create and inspect a session:

   ```sh
   thurbox-cli session create --name remote-work --host devbox \
     --repo-path /srv/project --agent codex
   thurbox-cli session list
   ```

   Substitute the agent you configured in `agents.toml`. For a first transport
   check without agent credentials, use `--agent shell`. To use the TUI, press
   `Ctrl+N`, choose **devbox** in the host picker, then choose a repository on
   that host and an agent. The session list marks off-local sessions with `☁`.

After a local Thurbox restart, the remote tmux window keeps running and the
session is restored in the list. Check with `thurbox-cli session list`; use
`thurbox-cli session sync --host devbox` to refresh sessions created on the
host itself. If a connection fails, run `ssh -v devbox true` first. If SSH
succeeds but creation fails, repeat the remote prerequisite command and check
the repository's path and agent authentication. A shared host needs a
compatible remote platform for CLI provisioning. Keep the host online when
first creating a session.

## Other OpenSSH routes

Each example below replaces the `devbox` SSH alias above. Put its matching
`[[hosts]]` block in `hosts.toml`, restart Thurbox, then run the shown SSH
check **before** session creation. The Linux host still needs the same Git,
tmux, agent and repository setup as [the common path](#plain-linux-ssh).

### SSH bastion

The operator must give you SSH access to both the jump host and the private
host, and allow the jump host to reach the private host's SSH port. OpenSSH's
[`ProxyJump` documentation](https://man.openbsd.org/ssh_config#ProxyJump)
defines this route.

```sshconfig
Host jump
    HostName jump.example.com
    User jump-user

Host private-dev
    HostName private-dev.internal.example.com
    User remote-user
    ProxyJump jump
```

```toml
[[hosts]]
name = "private-dev"
destination = "private-dev"
```

```sh
ssh private-dev 'printf ready'
thurbox-cli session create --name remote-work --host private-dev --repo-path /srv/project --agent codex
thurbox-cli session list
```

### AWS Systems Manager

The operator must enable an EC2 managed node with running SSH and a compatible
SSM Agent, Session Manager endpoints, and IAM permission for
`ssm:StartSession` with `AWS-StartSSHSession`. Locally, install AWS CLI and the
Session Manager plugin, authenticate with your organization's AWS profile, and
have an SSH key accepted by the instance. Follow the [AWS SSH over Session
Manager guide](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-getting-started-enable-ssh-connections.html)
and its [CLI options](https://docs.aws.amazon.com/cli/latest/reference/ssm/start-session.html).
**Session Manager command logging is unavailable for SSH sessions**: Session
Manager sees an encrypted tunnel, not the SSH commands.

In `~/.ssh/config`, replace `i-EXAMPLE` with your instance ID and choose its
actual Linux user, `PROFILE_NAME` with your authenticated profile, and `REGION`
with the instance's region.

```sshconfig
Host aws-dev
    HostName i-EXAMPLE
    User ec2-user
    IdentityFile ~/.ssh/ec2_key
    ProxyCommand sh -c "aws ssm start-session --profile PROFILE_NAME --region REGION --target %h --document-name AWS-StartSSHSession --parameters 'portNumber=%p'"
```

```toml
[[hosts]]
name = "aws-dev"
destination = "aws-dev"
```

```sh
ssh aws-dev 'printf ready'
thurbox-cli session create --name remote-work --host aws-dev --repo-path /srv/project --agent codex
thurbox-cli session list
```

The proxy command syntax comes from AWS's guide; this cloud connection was
source-verified, not executed here.

### Google Cloud IAP

The operator must allow IAP TCP forwarding to the VM's SSH port, grant the
needed IAP and VM login permissions, and configure SSH keys or OS Login.
Install and authenticate `gcloud` locally. Google's [OpenSSH client
instructions](https://docs.cloud.google.com/compute/docs/connect/ssh-using-iap)
first use `gcloud compute ssh` to establish a key, then use
`start-iap-tunnel --listen-on-stdin` as an OpenSSH proxy.

```sh
gcloud compute ssh VM_NAME --project=PROJECT_ID --zone=ZONE --tunnel-through-iap
```

After that succeeds, add to `~/.ssh/config` (replace all uppercase values):

```sshconfig
Host gcp-dev
    HostName VM_NAME
    User VM_USER
    IdentityFile ~/.ssh/google_compute_engine
    ProxyCommand gcloud compute start-iap-tunnel %h %p --listen-on-stdin --project=PROJECT_ID --zone=ZONE --verbosity=warning
```

```toml
[[hosts]]
name = "gcp-dev"
destination = "gcp-dev"
```

```sh
ssh gcp-dev 'printf ready'
thurbox-cli session create --name remote-work --host gcp-dev --repo-path /srv/project --agent codex
thurbox-cli session list
```

The `gcloud` commands were checked against Google documentation, not run
against a project here.

### Azure Bastion

The operator must deploy Bastion **Standard or Premium** with native client
support, grant the needed Azure Reader roles, and allow Bastion to reach the
VM's SSH port. Install and sign in to Azure CLI with the Bastion extension;
select the subscription containing the Bastion resource. See Microsoft's
[native client prerequisites](https://learn.microsoft.com/en-us/azure/bastion/native-client)
and [Linux tunnel command](https://learn.microsoft.com/en-us/azure/bastion/connect-vm-native-client-linux#connect-to-a-vm---tunnel-command).
The tunnel occupies one local port and must stay running while Thurbox uses it.

In a separate terminal, replace the placeholders and leave this running:

```sh
az login
az account set --subscription SUBSCRIPTION_ID
az network bastion tunnel --name BASTION_NAME --resource-group RESOURCE_GROUP \
  --target-resource-id VM_RESOURCE_ID --resource-port 22 --port 2222
```

Then add to `~/.ssh/config`:

```sshconfig
Host azure-dev
    HostName 127.0.0.1
    Port 2222
    User VM_USER
    IdentityFile ~/.ssh/azure_vm_key
    HostKeyAlias azure-dev-vm
```

```toml
[[hosts]]
name = "azure-dev"
destination = "azure-dev"
```

```sh
ssh azure-dev 'printf ready'
thurbox-cli session create --name remote-work --host azure-dev --repo-path /srv/project --agent codex
thurbox-cli session list
```

The Bastion tunnel syntax was checked against Microsoft documentation, not
executed against an Azure subscription here. Keep normal SSH host-key
verification for the VM behind the loopback tunnel.

## WSL

On Windows, Thurbox discovers other WSL distros automatically; no SSH server or
`hosts.toml` entry is needed. Install Git, tmux and your agent **inside the
distro** and put the repository on its Linux filesystem. Press `Ctrl+N` and
choose the distro, or use `--host DISTRO_NAME` with its discovered name. From
inside a distro, the current distro is local and sibling distros are remote
options. See [the WSL host reference](CONFIG.md#hoststoml).

Optional multiplexers such as RMUX and Herdr are not part of this remote setup
path. These playbooks use the currently supported tmux SSH/WSL backend.
