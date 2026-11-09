# Recorded Discord payloads

Synthetic envelopes in the shape a Discord listener hands to `fleet gateway
receive`, `{"dispatch": {op, t, s, d}, "parentId"}`, with made-up snowflakes,
read by the Discord adapter's unit tests through `event`. `dispatch` is a
Gateway dispatch as Discord documents it; `parentId` is the parent channel the
listener adds for a message in a thread, and `null` for a message in a
channel. None was recorded from a real guild.
