# Exact feasible false-negative witness

This preserves the skeptical reviewer's exact three-stack case. It is a
regression in `exact_reviewer_witness_is_a_safe_feasible_false_negative`.

```json
{
  "max_height": 3,
  "hazardous_exclusions": {},
  "stacks": [
    {"id":"A","x":0,"y":0,"reefer_socket":false,"frozen":false,"neighbors":[],"containers":[
      {"id":"P","weight_class":5,"reefer":false,"hazardous_group":null,"customs_hold":false},
      {"id":"X2","weight_class":4,"reefer":false,"hazardous_group":null,"customs_hold":false},
      {"id":"X1","weight_class":2,"reefer":false,"hazardous_group":null,"customs_hold":false}
    ]},
    {"id":"B","x":1,"y":0,"reefer_socket":false,"frozen":false,"neighbors":[],"containers":[
      {"id":"B0","weight_class":5,"reefer":false,"hazardous_group":null,"customs_hold":false}
    ]},
    {"id":"C","x":2,"y":0,"reefer_socket":false,"frozen":false,"neighbors":[],"containers":[
      {"id":"C0","weight_class":3,"reefer":false,"hazardous_group":null,"customs_hold":false}
    ]}
  ]
}
```

```json
{"pickups":["P"]}
```

The bounded heuristic chooses nearer `B` for light blocker `X1`. Neither `B`
nor `C` can then accept heavier `X2`, so it safely returns review. This legal
witness passes full metadata verification and transition replay:

1. Relocate `X1` from `A` to `C`.
2. Relocate `X2` from `A` to `B`.
3. Pick up `P` from `A`.

This proves the prototype cannot claim complete feasible-wave planning or treat
its review as proof that a wave is infeasible.
