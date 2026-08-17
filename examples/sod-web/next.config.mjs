/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "standalone",
  // the addon package wraps a native .node binary — never bundle it
  serverExternalPackages: ["sod-web-addon"],
};

export default nextConfig;
