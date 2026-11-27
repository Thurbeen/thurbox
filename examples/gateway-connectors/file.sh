#!/bin/sh

set -u

answer() {
	printf '%s\n' "$1"
	exit 0
}

unknown() {
	answer "$(jq -cn --arg why "$1" '{unknown: {why: $why}}')"
}

command -v jq >/dev/null 2>&1 || answer '{"unknown":{"why":"jq is not installed"}}'

verb=${1:-}
req=$(cat)
printf '%s' "$req" | jq -e 'type == "object"' >/dev/null 2>&1 ||
	unknown "the request is not a JSON object"
[ "$(printf '%s' "$req" | jq -r '.protocol // empty')" = 1 ] ||
	unknown "this connector speaks protocol 1"

field() {
	printf '%s' "$req" | jq -r "$1"
}

platform=$(field '.account.platform // empty')
[ -n "$platform" ] || unknown "the request names no account platform"
dir=$(field '.account.options.dir // empty')
[ -n "$dir" ] || unknown "the account sets no options.dir"
mkdir -p "$dir" || unknown "cannot make $dir"
messages="$dir/messages.jsonl"
touch "$messages" || unknown "cannot write $messages"
printf '%s\n' "$(printf '%s' "$req" | jq -c --arg verb "$verb" '{verb: $verb, account: .account}')" >>"$dir/requests.jsonl"

TO_MESSAGE='def to_message($platform; $bot):
  {conversation: {platform: $platform, chat: .channel, thread: (.thread // .id)},
   id: .id,
   author: {id: .user, name: (.name // .user),
            kind: (if .user == $bot then "fleet" else "person" end)},
   mentionsFleet: (.user != $bot and ((.text // "") | contains("<@" + $bot + ">"))),
   at: .ts,
   text: (.text // ""),
   startsThread: ((.thread // .id) == .id)}
  + (if .key then {key: .key} else {} end);'

bot=$(field '.account.bot')

lock() {
	exec 9>"$dir/.lock"
	if command -v flock >/dev/null 2>&1; then
		flock 9
	fi
}

now_ms() {
	jq -n 'now * 1000 | floor'
}

place() {
	channel=$1
	thread=$2
	text=$3
	key=$4
	existing=$(jq -c --arg key "$key" 'select(.key == $key)' "$messages" | head -n 1)
	if [ -n "$existing" ]; then
		printf '%s' "$existing"
		return
	fi
	at=$(now_ms)
	id=$(jq -n --argjson at "$at" --argjson n "$(wc -l <"$messages")" '($at + $n) | tostring')
	[ -n "$thread" ] || thread=$id
	record=$(jq -cn --arg id "$id" --arg channel "$channel" --arg thread "$thread" \
		--arg user "$bot" --arg text "$text" --arg key "$key" --argjson ts "$at" \
		'{id: $id, channel: $channel, thread: $thread, user: $user, text: $text, ts: $ts, key: $key}')
	printf '%s\n' "$record" >>"$messages"
	printf '%s' "$record"
}

case $verb in
manifest)
	answer "$(jq -cn --arg platform "$platform" '{known: {protocol: 1, platform: $platform,
		verbs: ["manifest", "parse", "event", "whoami", "capabilities", "post", "open", "history"]}}')"
	;;
parse)
	answer "$(printf '%s' "$req" | jq -c --arg platform "$platform" '
		(.address | split(":")) as $p
		| if ($p | length) < 2 or ($p | length) > 3 or $p[0] != $platform or any($p[]; . == "")
		  then {unknown: {why: "\(.address) is not \($platform):<channel>[:<thread>]"}}
		  else {known: ({platform: $p[0], chat: $p[1]} + (if $p[2] then {thread: $p[2]} else {} end))}
		  end')"
	;;
event)
	answer "$(printf '%s' "$req" | jq -c --arg platform "$platform" --arg bot "$bot" "$TO_MESSAGE"'
		.envelope as $e
		| if ($e.payload.message | type) != "object" then {known: null}
		  else {known: {id: $e.payload.delivery, verified: $e.verified, receivedAt: $e.receivedAt,
		                event: ({type: "message"} + ($e.payload.message | to_message($platform; $bot)))}}
		  end')"
	;;
whoami)
	answer "$(jq -cn --arg bot "$bot" '{known: {id: $bot, name: "file connector"}}')"
	;;
capabilities)
	answer '{"known":{"history":true,"marker":"metadata","unmentioned":true,"threads":true,"reactions":false}}'
	;;
post | open)
	lock
	channel=$(field 'if .conversation then .conversation.chat else .channel.chat end')
	thread=$(field '.conversation.thread // empty')
	record=$(place "$channel" "$thread" "$(field '.text')" "$(field '.intentKey')")
	if [ "$verb" = post ]; then
		answer "$(printf '%s' "$record" | jq -c '{known: {type: "posted", message: .id, at: .ts}}')"
	fi
	answer "$(printf '%s' "$record" | jq -c --arg platform "$platform" \
		'{known: {type: "opened", conversation: {platform: $platform, chat: .channel, thread: .thread},
		          message: .id, at: .ts}}')"
	;;
history)
	answer "$(jq -cs --argjson req "$req" --arg platform "$platform" --arg bot "$bot" "$TO_MESSAGE"'
		$req.conversation as $c | $req.query as $q
		| map(select(.channel == $c.chat and (.thread // .id) == $c.thread))
		| sort_by(.ts)
		| map(to_message($platform; $bot) | del(.conversation))
		| if $q.type == "since" then map(select(.at >= $q.at))
		  elif $q.id then (map(.id) | index($q.id)) as $i | if $i == null then . else .[$i + 1:] end
		  else . end
		| if $q.limit then .[:$q.limit] else . end
		| {known: {messages: ., leftOut: 0}}' "$messages")"
	;;
*)
	unknown "this connector has no verb ${verb:-(none)}"
	;;
esac
