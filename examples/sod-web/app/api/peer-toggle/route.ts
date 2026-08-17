import { NextRequest, NextResponse } from "next/server";
import { getSod } from "@/lib/sod";

export async function POST(req: NextRequest) {
  const { url, paused } = await req.json();
  if (typeof url !== "string" || typeof paused !== "boolean") {
    return NextResponse.json({ error: "url and paused required" }, { status: 400 });
  }
  const found = getSod().loop.setPaused(url, paused);
  if (!found) {
    return NextResponse.json({ error: "unknown peer" }, { status: 404 });
  }
  return NextResponse.json({ ok: true });
}
