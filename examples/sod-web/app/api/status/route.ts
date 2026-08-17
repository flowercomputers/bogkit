import { NextResponse } from "next/server";
import { getSod } from "@/lib/sod";

export const dynamic = "force-dynamic";

export async function GET() {
  const sod = getSod();
  const s = sod.addon.status();
  return NextResponse.json({
    ...s,
    serveAddr: sod.serveAddr,
    peers: sod.loop.peers,
    online: sod.loop.online(),
    hasPeers: sod.loop.peers.length > 0,
  });
}
