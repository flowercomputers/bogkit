import { NextResponse } from "next/server";
import { getSod } from "@/lib/sod";

export const dynamic = "force-dynamic";

export async function GET() {
  return NextResponse.json(getSod().addon.board());
}
