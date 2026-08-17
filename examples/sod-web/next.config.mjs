/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "standalone",
  // the addon package wraps a native .node binary — never bundle it.
  // serverExternalPackages alone doesn't survive the file: symlink
  // (webpack resolves to the real path outside node_modules), so mark it
  // external at the webpack layer too.
  serverExternalPackages: ["sod-web-addon"],
  webpack: (config, { isServer }) => {
    if (isServer) {
      config.externals.push({ "sod-web-addon": "commonjs sod-web-addon" });
    }
    return config;
  },
};

export default nextConfig;
