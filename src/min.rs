use std::{io::{stdout, Write}, hash::Hash};
use anyhow::Result;
use crossterm::{cursor::{MoveTo, MoveToNextLine}, execute, style::{StyledContent, Stylize}, terminal::{Clear, ClearType}};
use rand::random_range;
use hashbag::HashBag;

fn within_range(ry1: u16, ry2: u16, ro: u16, rx: u16, cy: u16, cx: u16) -> bool {
  cx == rx && (cy >= ry1 + ro) && (cy <= ry2 + ro)
}
fn make_hashbag<T: IntoIterator>(items: T) -> HashBag<T::Item>
  where T::Item: Hash + Eq {
  let mut bag = HashBag::new();
  for i in items { bag.insert(i); }
  bag
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Component {
  Red,
  Green,
  Blue,
  Attack,
  Block,
  Buff,
  Debuff,
  Stun
}
impl Component {
  fn to_string(&self) -> String {
    match *self {
      Self::Red => "Red",
      Self::Green => "Green",
      Self::Blue => "Blue",
      Self::Attack => "Attack",
      Self::Block => "Block",
      Self::Buff => "Buff",
      Self::Debuff => "Debuff",
      Self::Stun => "Stun",
    }.to_string()
  }
  fn get_description(&self) -> String {
    match self {
      Self::Red => "Fast speed, physical type. Chaos and momentum.".to_string(),
      Self::Green => "Normal speed, healing type. Protection and trickery.".to_string(),
      Self::Blue => "Slow speed, magical type. Deterrents and destruction.".to_string(),
      other => Skill::craft(&make_hashbag([other.clone()])).unwrap().description
    }
  }
  fn stylize(&self) -> StyledContent<String> {
    match *self {
      Self::Red => "o".to_string().red(),
      Self::Green => "o".to_string().green(),
      Self::Blue => "o".to_string().blue(),
      _ => self.to_string().white()
    }
  }
  fn get_cost(&self) -> i32 {
    if self.is_color() { 1 } else { 2 }
  }
  fn is_color(&self) -> bool {
    *self == Self::Red || *self == Self::Green || *self == Self::Blue
  }
  fn random_color() -> Self {
    let colors = vec![Self::Red, Self::Green, Self::Blue];
    colors[random_range(0..colors.len())].clone()
  }
  fn random_skill() -> Self {
    let skills = vec![Self::Attack, Self::Block, Self::Buff, Self::Debuff, Self::Stun];
    skills[random_range(0..skills.len())].clone()
  }
}
struct Skill {
  name: String,
  description: String,
  components: HashBag<Component>
}
impl Skill {
  fn get_all_recipes() -> Vec<Skill> {
    let make_skill = |name: &str, description: &str, items: [_; _]| {
      let mut bag = HashBag::new();
      for i in items { bag.insert(i); }
      Skill { name: name.to_string(), description: description.to_string(), components: bag }
    };
    vec![
      make_skill("Attack", "Deal 80% of Red power as Red damage", [Component::Attack]),
      make_skill("Block", "no idea tbh", [Component::Block]),
      make_skill("Buff", "no idea tbh 2", [Component::Buff]),
      make_skill("Debuff", "no idea tbh 3", [Component::Debuff]),
      make_skill("Stun", "no idea tbh 4", [Component::Stun]),
    ]
  }
  fn craft(components: &HashBag<Component>) -> Option<Self> {
    for i in Self::get_all_recipes() {
      if *components == i.components { return Some(i); }
    }
    None
  }
}
pub struct MinimalGameState {
  vbox: Vec<Component>,
  bits: i32
}

impl MinimalGameState {
  pub fn new() -> Self {
    // create a new vbox and add random colors and skills to it
    let mut vbox = vec![];
    for _i in 0..6 {
      vbox.push(Component::random_color());
    }
    for _i in 0..3 {
      vbox.push(Component::random_skill());
    }
    let bits = 40;
    MinimalGameState { vbox, bits }
  }
  pub fn ui(&self, term_cols: u16, term_rows: u16, cursor_col: u16, cursor_row: u16) -> Result<()> {
    let mut stdout = stdout();
    // draw the minimal border
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
    write!(stdout, "┌ minimal {}┐", "─".repeat((term_cols - 11).into()))?;
    for _i in 1..(term_rows-1) {
        execute!(stdout, MoveToNextLine(1))?;
        write!(stdout, "│{}│", " ".repeat((term_cols - 2).into()))?;
    }
    execute!(stdout, MoveTo(0, term_rows-1))?;
    write!(stdout, "└{}┘", "─".repeat((term_cols - 2).into()))?;
    let mut hovered_name = "".to_string();
    let mut hovered_desc = "".to_string();
    // draw the VBOX's colors!!
    for (i, component) in self.vbox.iter().filter(|c| c.is_color()).enumerate() {
      let ii = i as u16;
      execute!(stdout, MoveTo(11 + ii * 4, 1))?;
      write!(stdout, "{}", if self.bits < component.get_cost() { component.stylize().crossed_out() } else {
        if within_range(11, 14, ii * 4, 1, cursor_col, cursor_row) {
          hovered_name = component.to_string();
          hovered_desc = component.get_description();
          component.stylize().bold()
        } else { component.stylize() }
      })?;
    }
    // and draw the skills too
    for (i, component) in self.vbox.iter().filter(|c| !c.is_color()).enumerate() {
      let ii = i as u16;
      execute!(stdout, MoveTo(11 + ii * 9, 2))?;
      write!(stdout, "{}", if self.bits < component.get_cost() { component.stylize().crossed_out() } else {
        if within_range(11, 19, ii * 9, 2, cursor_col, cursor_row) {
          hovered_name = component.to_string();
          hovered_desc = component.get_description();
          component.stylize().bold()
        } else { component.stylize() }
      })?;
    }
    // draw the hovered item's description
    execute!(stdout, MoveTo(40, 1))?;
    write!(stdout, "{}", hovered_name.bold())?;
    for i in 0..3 {
      // let's just assume it won't be more than like 3 lines long
      execute!(stdout, MoveTo(40, 2 + i))?;
      // get the relevant part of the string and print it
      if(hovered_desc.len() > 18) { write!(stdout, "{}", hovered_desc.drain(..18).collect::<String>())?; }
      else { write!(stdout, "{}", hovered_desc)?; }
    }
    // draw the current money and the refund button
    execute!(stdout, MoveTo(2, 1))?;
    write!(stdout, "{}B", self.bits)?;
    execute!(stdout, MoveTo(2, 2))?;
    write!(stdout, "{}", "refund".dark_grey())?; // todo: color this based on whether something refundable is being held
    Ok(())
  }
}