#!/usr/bin/env python3
"""Terminal history sync: capture cursor before snapshot; refetch on change/reset."""
import argparse
import json
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts/cloud'))
from bog_client import Client, Failure


def snapshots(client, limit=100):
    cursor = client.wait(timeout=0)['cursor']
    yield client.top('recent', limit=limit, include_fields=['/role', '/content', '/created_at'])
    while True:
        change = client.wait(cursor)
        cursor = change['cursor']
        if change['changed'] or change['reset']:
            yield client.top('recent', limit=limit, include_fields=['/role', '/content', '/created_at'])


if __name__ == '__main__':
    p=argparse.ArgumentParser();p.add_argument('--config',required=True);a=p.parse_args()
    try:
        for history in snapshots(Client.from_config(a.config)): print(json.dumps(history),flush=True)
    except KeyboardInterrupt: pass
    except (Failure,OSError,ValueError):
        print('History sync stopped; check connection and private configuration.',file=sys.stderr);sys.exit(1)
