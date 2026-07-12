# Area Denial Location-Based MMO

The world’s first online, location-based, area denial contact sport. It’s for a hackathon project, so I don’t want to make a whole iphone app and release it (yet). It uses Flower Computer’s set of Bogkit database tools heavily (important).

Battles:
- The system selects an area, with a bounding box as a battleground. For the purposes of this hackathon, the locations the system can pick from are New York city parks. We use a copy of the foursquare OS places dataset to find the areas: https://places.foursquare.com/dataset/OS%20Places/details
- Battles last 3 hours.
- At the end of the battle, the team with the highest percentage of its members inside the bounding box wins.
- If a team can completely remove the opposing team from the area, the clock stops and they win.

- User gets set up, chooses between one of two teams. For the purposes of this hackathon project it happens in browser.
    - Let’s name the teams as the two ends of the G train for now, Team Court Square and Team Church Ave.
    - The user can see the team member count.
    - A game can’t start unless each team has at least one member.
- The user opens a link on their phone, and begins streaming their location in some manner.

- Don’t focus on interface design, use raw/barely-styled components for any frontends for now.

There is no easy way for players to identify each other, and games take place in larger public areas, so players will need to independently discover who is playing, who is on their team, and who is on the opposite team. This is by design, to enable more creative forms of hiding and subterfuge. Players can use any method at their disposal to get members of the opposing team to leave the premises.