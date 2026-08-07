# Catalog compiler prototype fixtures

The format is deliberately small and dependency-free:

~~~text
locale fr
fallback en-US
message apples
plural count
branch one
text Une pomme pour {name}
branch other
text {count} pommes pour {name}
end
~~~

"text" supports "{variable}" placeholders. "plural" selects "one" for the
value 1 and "other" otherwise. "select" chooses an exact branch and then
"other". "ref <locale> <message-id>" is a nested fallback reference.

The prototype validates localized message shapes against en-US, while a
missing whole message continues to use the catalog's fallback chain. The
emitted runtime table is the same grammar, normalized by locale, message ID,
and branch name, which makes repeated output byte-identical.
