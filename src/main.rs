use anyhow::Result;

mod browser;
mod challenge;
mod scrappey;

#[tokio::main]
async fn main() -> Result<()> {
    let mut browser = browser::Browser::new();

    match browser.load_data("browser_data.json") {
        Ok(_) => println!("Browser data loaded successfully."),
        Err(e) => println!("Failed to load browser data: {}", e),
    }

    // get url from user input
    println!("Enter the URL to navigate to:");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    let url = input.trim().to_string();
    if url.is_empty() {
        println!("No URL provided. Exiting.");
        return Ok(());
    }

    match browser.navigate(&url).await {
        Ok(response) => {
            println!("Navigated to: {}", response.url);
            println!("Status: {}", response.status);
            println!("Body: {}", response.body);
            println!("User Agent: {}", response.user_agent);
        }
        Err(e) => {
            println!("Failed to navigate: {}", e);
        }
    }

    // Save browser data after navigation
    match browser.save_data("browser_data.json") {
        Ok(_) => println!("Browser data saved successfully."),
        Err(e) => println!("Failed to save browser data: {}", e),
    }

    Ok(())
}
