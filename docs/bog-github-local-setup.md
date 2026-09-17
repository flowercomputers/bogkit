# Local GitHub setup handoff

Give the following instructions to an agent on the Mac you can access directly. The app registration is organization-owned and independent of which Mac creates it.

> Set up direct GitHub sign-in for Bog Cloud. GitHub supplies identity; Bog issues and manages its own API tokens. Do not use WorkOS.
>
> Open https://github.com/organizations/flowercomputers/settings/applications in my local browser. Reuse an existing matching Bog Cloud OAuth app if its homepage and callback match; otherwise create one owned by flowercomputers:
>
> - Name: Bog Cloud
> - Homepage: https://flower-bog-cloud.fly.dev/
> - Callback: https://flower-bog-cloud.fly.dev/auth/callback
> - Description: Sign in to Bog Cloud to create and manage small databases for your apps and agents. GitHub is used only to verify identity.
> - Wildcard callbacks: off.
> - GitHub Device Flow: off; Bog supplies its own approval-link flow.
> - Expiring GitHub access tokens: on.
> - No repository permissions and no GitHub App installation.
>
> Obtain the client ID and generate a client secret, handing credential generation to me if your tools require it. Save BOG_GITHUB_CLIENT_ID, BOG_GITHUB_CLIENT_SECRET and BOG_GITHUB_REDIRECT_URI in ~/.config/bog-cloud/github.env. The redirect URI is the exact callback above. Use directory permissions 700 and file permissions 600. Do not print the secret, include it in chat or command arguments, or commit it.
>
> If an existing, verified SSH connection to my build Mac, violaceae, is available, securely transfer the file to /Users/edouard/.config/bog-cloud/github.env there with private permissions. The build account is edouard and its hostname is verified as violaceae. Do not guess the destination or change SSH/security settings. Otherwise retain it locally and report that transfer is pending.
>
> Do not deploy, set Fly secrets, activate signup, rotate the existing owner token or modify the chat app. Report only the app settings URL, public client ID, and whether private storage and transfer succeeded. Never report the client secret.

## Handoff status

The local agent reports that the organization-owned app has been created at https://github.com/organizations/flowercomputers/settings/applications/3864757. The callback and flags match these instructions. The owner subsequently supplied the configuration, which was saved privately and installed through Fly Secrets. Real GitHub sign-in and device approval now pass. Do not create another app; this handoff is complete.
