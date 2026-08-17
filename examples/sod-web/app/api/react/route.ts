import { NextRequest, NextResponse } from "next/server";
import { getSod } from "@/lib/sod";

export async function POST(req: NextRequest) {
  const { emoji } = await req.json();
  if (typeof emoji !== "string" || !emoji) {
    return NextResponse.json({ error: "emoji required" }, { status: 400 });
  }
  const sod = getSod();
  sod.addon.react(emoji);
  sod.loop.kick();
  return NextResponse.json({ ok: true });
}
