# Playbook title

## Inputs

Describe the requested outcome, resource kinds, configuration references,
required policies and authorization boundary. Keep secrets in providers.

## Work

Create the desired resources with `fleet apply -f FILE`. Inspect their status
with `fleet status KIND NAME --json`. Controllers reconcile under
`fleet up`; use bounded `fleet reconcile` while `up` is stopped. A worker
reports a claim with `fleet result`; Fleet checks its generation and output
shape, not its truth.

## Acceptance

Name the observed behavior, failure cases and evidence that establish the
outcome. Report native platform gaps explicitly. A run does not authorize
publication or live cutover beyond its brief.
