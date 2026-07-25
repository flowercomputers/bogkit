# Untitled Mobile FPS

This plan describes a new kind of Mobile FPS that is played with the phone's camera and the user's hand, in which players will be matched with one another, and will need to find each other, and shoot at the other player by making a finger-guns gesture at their target and putting the thumb down to "pull the trigger". This initial run will serve as a tech demo, but it should be polished and reliable enough to demo live.

The game should be able to save the player's appearance (face, outfit) before a match, so that they can be recognized by opponent's phones.

We're targeting iOS for the client software.

The plan should use Flower Computer Company's Bogkit set of high-speed database tools wherever possible – I have it cloned at ~/Development/bogkit

## Phase 1: Finger-Gun firing.

The first phase is a tech demo of the finger-gun firing gesture recognition.

Here are examples of the finger gun:
- <https://upload.wikimedia.org/wikipedia/commons/thumb/1/1c/Action%21_%28Unsplash%29.jpg/3840px-Action%21_%28Unsplash%29.jpg>
- <https://c8.alamy.com/comp/2S6YY5A/hand-finger-gun-isolated-on-white-background-2S6YY5A.jpg>
- <https://thumbs.dreamstime.com/b/stylized-illustration-pair-hands-doing-finger-guns-gesture-first-person-point-view-st-pov-mock-gun-fight-isolated-315364100.jpg>

In this phase, the product should be an iPhone application that opens the camera, and when the user has their hand extended in finger-gun form in the frame, the spot they are aiming at should have a small red laser dot superimposed over it. Putting the thumb of the finger gun down should fire the gun, producing a muzzle flash.

Keep the view in a debug mode with bounding boxes for now.

## Phase 2: Backend and Gameplay Loop

This phase of the plan should be significantly more involved, and will likely necessitate changes to the firing mechanic. You will likely need to create backend servers, using bogkit tech somewhere in there.

Users will need to register their appearance into the app so they can become valid targets in the shooter game. They will likely need to take full-body mirror selfies and face pics to accomplish this.

Users of the app should be able to begin gameplay and either:
A) Invite a friend, another user of the app, via their contacts to play a match live.
or
B) Join a match against random players within a ~2mi radius, waiting to be matched with one another.

When a match starts, a player will be shown a police sketch-style face pic of their opponent(s). They will then be shown a radar view, that shows their approximate position, relative to the other players. When they are in range of another player's phone, the players will be alerted, camera mode will be entered, with the radar becoming a small circular overlay in the upper left corner of the screen, and more detailed directional information will be used. The phone's camera view will overlay a solid-color red silhouette over the target. Successfully aiming and firing a finger gun at the target will land a shot on the opponent. An opponent will go down in 3 shots. After an opponent is killed, their silhouette fades to grey. Users have the option to save the last ~15s of their gameplay to their camera roll as a first-person-shooter style "killcam replay".

There should be a list of previously played matches available in the menu.

This should support matches of up to 3v3.
