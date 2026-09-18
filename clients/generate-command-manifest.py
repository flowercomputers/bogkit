#!/usr/bin/env python3
"""Derive supported CLI route metadata from the shared server operation table."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
ROOT=Path(__file__).resolve().parents[1]
COMMANDS={
 'resources':'list_resources','routes':'list_routes','get':'get_record','put':'upsert_record',
 'remove':'delete_record','batch':'batch','batch-get':'query_resource','query':'query_resource',
 'list':'query_resource','top':'query_resource','search':'search_resource','wait':'wait_for_change',
 'request-status':'request_info','explain':'request_info','create':'create_bog',
 'cleanup-preview':'preview_cleanup','cleanup-execute':'execute_cleanup',
 'add-search':'apply_definition_update',
}
source=(ROOT/'cloud/src/contract.rs').read_text().split('pub const OPERATIONS:',1)[1].split('\n];',1)[0]
operations={name:{'operation':name,'method':method,'path':path} for name,method,path in re.findall(r'\(\s*"([a-z_]+)",\s*"([A-Z]+)",\s*"([^"]+)"',source)}
value={'source':'cloud/src/contract.rs::OPERATIONS','commands':{command:operations[name] for command,name in COMMANDS.items()},'local_workflows':{'connect':'Device authorization saved privately','install':'Private one-use app-access redemption','delete-sandbox':'DELETE /v1/bogs/{bog_id}, confirm exact ID; server enforces ownership','diagnostics':'GET /v1/bogs/{bog_id}/usage'}}
text=json.dumps(value,indent=2)+'\n';output=ROOT/'clients/command-manifest.json'
p=argparse.ArgumentParser();p.add_argument('--check',action='store_true');args=p.parse_args()
if args.check:
 if output.read_text()!=text: raise SystemExit('Command manifest drifted; run clients/generate-command-manifest.py')
 for command in ([sys.executable,str(ROOT/'scripts/cloud/bog_cli.py'),'--help'],['node',str(ROOT/'clients/typescript/src/cli.js'),'--help']):
  help_text=subprocess.check_output(command,text=True)
  for name in value['commands']:
   if not re.search(r'(?<![a-z-])'+re.escape(name)+r'(?![a-z-])',help_text): raise SystemExit('CLI command missing: '+name)
else:output.write_text(text)
