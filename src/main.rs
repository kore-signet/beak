use beak::*;

fn main() {
    run(
        4,
        "localhost:8000",
        200000,
        &[&NyaHandler],
        ()
    ).unwrap();
}


fn handle<'a, 'b, 'c>(request: Request<'a, 'b, 'c>, context: ()) -> BeakResult<()> {
    request.respond(200, vec![], |w, _| {
        writeln!(w, "owo")
    }).unwrap();

    Ok(())
}

fn_to_handler!(NyaHandler with context (); "/nya" => handle);